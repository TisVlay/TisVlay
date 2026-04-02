# Issue: System tray nested submenus collapse on first expansion

## Environment

- KDE Plasma 6.6.3
- kstatusnotifieritem 6.24.0
- plasma-workspace 6.6.3
- Qt 6.8 (with commit `353ce53` — "QMenu: don't show an empty menu")
- ksni 0.3 (Rust StatusNotifierItem crate)
- NixOS, nixpkgs rev `b40629efe5d6`

## Symptom

When the tray app starts and shows a TV as connected, opening a nested
submenu (e.g., Profile or Input inside the TV submenu) causes the entire
TV submenu to collapse on the **first** expansion. After this transient
failure, reopening the same nested submenu works correctly for the rest
of the session. The nested submenu is briefly rendered in full before
collapsing — it's not that the content is missing, but the menu is
dismissed almost immediately.

## Debugging methodology

### D-Bus signal monitoring

Used `dbus-monitor --session "interface='com.canonical.dbusmenu'"` to
capture all interactions between the KDE system tray host and the ksni
tray app. This revealed:

1. The KDE host (`:1.27`) sends `AboutToShow(id)` + `Event("opened")`
   + `GetLayout(id)` on first submenu expansion
2. On subsequent expansions: `AboutToShow` only (no `GetLayout` — cached)
3. The host sends unsolicited `Event("closed")` on both parent and child
   submenus ~0.3-1s after first expansion, **without any signal from ksni**

### ksni source analysis

Reviewed the ksni library at `/home/ruffsl/ws/lgtv/libs/ksni/`:

- `about_to_show()` always returns `false` (no update needed)
- `event()` only handles `"clicked"` — ignores `"opened"/"closed"` entirely
- `update_menu()` diffs the menu and only emits `LayoutUpdated` when the
  child structure changes; emits `ItemsPropertiesUpdated` for property-only
  changes

### Tracing with instrumented code

Added per-field diff logging to the poll loop to detect which
`CachedTvState` fields triggered false state changes. This identified
the HashMap ordering issue (see fixes below).

## Fixes applied (tisvlay side)

### 1. Eager initial state fetch

**Problem:** The tray started with empty state and the first 5-second poll
populated profiles/inputs, triggering a `LayoutUpdated` signal that closed
any open submenus.

**Fix:** Fetch initial state (including eager session service connection)
before spawning the tray icon. The tray starts with full data, so the
first poll finds identical state and skips the update.

### 2. Shadow cache in poll loop

**Problem:** Every `handle.update()` call triggers ksni to diff the menu
and potentially emit `LayoutUpdated`, which closes open submenus.

**Fix:** Keep a shadow copy of the last fetched state in the poll loop.
Only call `handle.update()` when the new state differs from the shadow.
This prevents unnecessary menu rebuilds from polls that return the same
data.

### 3. BTreeMap for profile maps

**Problem:** `HashMap<String, ProfileConfig>` has non-deterministic
iteration order. Profile name lists (`available_system_profiles`,
`available_user_profiles`) returned in different order between polls,
causing the shadow cache comparison to detect false state changes.

**Fix:** Changed `SystemSection.profiles` and `UserSection.profiles` from
`HashMap` to `BTreeMap`. Iteration is now always sorted by key at the
source, eliminating ordering-dependent false positives in all consumers.

### 4. PartialEq on CachedTvState

Added `#[derive(PartialEq)]` to `CachedTvState` to enable the shadow
cache comparison. Tests verify that identical state compares as equal
and that ordering matters (BTreeMap ensures consistency).

## Remaining issue: KDE Qt bug

After all tisvlay-side fixes, the tray emits **zero `LayoutUpdated`
signals** during normal operation, yet nested submenus still collapse
on first expansion. The D-Bus trace confirms KDE sends unsolicited
`Event("closed")` without any provocation from ksni.

### Root cause: Qt 6.8 empty QMenu guard + async libdbusmenuqt

**KDE Bug 506439** — "Submenus don't show on mouseover"
https://bugs.kde.org/show_bug.cgi?id=506439

Qt commit `353ce53` ("QMenu: don't show an empty menu", QTBUG-129108)
added a guard that prevents QMenu popups with no visible actions. KDE's
`libdbusmenuqt` populates QMenu items **asynchronously** — it creates an
empty QMenu first, then fetches items via D-Bus (`GetLayout`). The Qt
guard closes the initially-empty submenu before the async population
completes.

On subsequent opens, the items are cached from the first D-Bus round-trip,
so the QMenu is no longer empty when Qt evaluates the guard — it displays
correctly.

### Fix status

**MR !5688** (Draft, closed in favor of !5763):
https://invent.kde.org/plasma/plasma-workspace/-/merge_requests/5688

**MR !5763** (replacement, open, targets master):
https://invent.kde.org/plasma/plasma-workspace/-/merge_requests/5763

A larger rewrite that replaces `libdbusmenuqt` with new `DBusMenuModel` +
`DBusMenuView` components. Key changes:
- Stops sending `AboutToShow` for unexpanded submenus (also fixes Firefox
  global menu, Mozilla bug 1975681)
- Reuses menu objects for submenus instead of recreating them
- Removes reliance on Qt widget-specific details that caused the empty
  QMenu race condition
- References BUG: 506439 (same root cause we identified)
- Removes search functionality (considered problematic with shallow menu
  layouts)

**No fix has shipped in any released Plasma version** as of 6.6.3. MR
!5763 is still open and not yet merged to master. The existing bug is
filed under kwin/appmenu — a separate bug should be filed under
plasmashell/System Tray widget to ensure visibility.

### Potential tisvlay workaround

ksni's `about_to_show()` could be modified to return `true` on the first
call for each submenu, forcing KDE to re-fetch the layout before
displaying. This might give `libdbusmenuqt` time to populate the QMenu
before Qt's empty-menu guard fires. However, this would trigger
`LayoutUpdated` which has its own side effects. Not yet tested.

## Related KDE bugs

| Bug | Description | Status |
|-----|-------------|--------|
| [506439](https://bugs.kde.org/show_bug.cgi?id=506439) | Submenus don't show on mouseover (appmenu) | REPORTED |
| [MR !5688](https://invent.kde.org/plasma/plasma-workspace/-/merge_requests/5688) | libdbusmenuqt prefetch fix (closed) | Closed → !5763 |
| [MR !5763](https://invent.kde.org/plasma/plasma-workspace/-/merge_requests/5763) | Replacement fix | Open |
| [484216](https://bugs.kde.org/show_bug.cgi?id=484216) | System Tray popups close immediately | RESOLVED FIXED (Plasma 6.1) |
| [QTBUG-129108](https://bugreports.qt.io/browse/QTBUG-129108) | QMenu: don't show an empty menu | Fixed in Qt 6.8 |

## Test reproduction

```bash
# Terminal 1: Monitor D-Bus signals
dbus-monitor --session "interface='com.canonical.dbusmenu'"

# Terminal 2: Run tray with debug logging
RUST_LOG=tisvlay_tray=debug,tisvlay_core::dbus_client=debug,zbus=warn,ksni=warn \
  cargo run --bin tisvlay-tray -- --user

# Steps:
# 1. Wait for tray icon to appear
# 2. Click tray icon to open menu
# 3. Hover over TV name to expand submenu
# 4. Hover over "Profile" or "Input" to expand nested submenu
# 5. Observe: submenu collapses on first expansion
# 6. Repeat step 4: submenu stays open on second expansion
#
# In dbus-monitor: look for LayoutUpdated signals (should be zero)
# and unsolicited Event("closed") from the KDE host
```
