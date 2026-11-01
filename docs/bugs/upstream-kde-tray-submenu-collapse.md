# Upstream: KDE's tray collapses a nested submenu on its first expansion

**Status:** open upstream, still reproducible on 2026-11-01 (Plasma 6.8.2, Qt 6.12). Nothing to file from our side; this tracks the upstream fix.

## Symptom

Soon after the tray starts, opening a nested submenu inside a TV's submenu (Profile or Input, say) collapses the whole TV submenu. It renders in full for a moment, then closes. Opening it again works for the rest of the session.

## Cause

Qt's "don't show an empty menu" guard ([QTBUG-129108](https://bugreports.qt.io/browse/QTBUG-129108), in Qt 6.8) closes a `QMenu` popup that has no visible actions. KDE's `libdbusmenuqt` builds the submenu empty and fills it asynchronously over D-Bus (`GetLayout`), so the guard closes it before the items arrive. The second time the items are cached, the menu isn't empty, and it stays open.

An earlier investigation, on Plasma 6.6.3, ruled out the tray's side: during normal operation the tray emits no `LayoutUpdated` signals, yet a D-Bus trace shows the KDE host sending an unprompted `Event("closed")`. Its write-up (with the tray-side fixes it made along the way) is in git history as `docs/kde/tray_issue.md`.

## Upstream

| Item | What | Status when last checked |
|---|---|---|
| [KDE 506439](https://bugs.kde.org/show_bug.cgi?id=506439) | Submenus don't show on mouseover (filed under appmenu, same root cause) | Reported |
| [MR !5688](https://invent.kde.org/plasma/plasma-workspace/-/merge_requests/5688) | `libdbusmenuqt` prefetch fix | Closed in favor of !5763 |
| [MR !5763](https://invent.kde.org/plasma/plasma-workspace/-/merge_requests/5763) | Replaces `libdbusmenuqt` with `DBusMenuModel` and `DBusMenuView`; stops sending `AboutToShow` for unexpanded submenus | Open |

A separate report under plasmashell's System Tray component would give it visibility there; none has been filed.

## Possible workaround

ksni's `about_to_show()` could return `true` on the first call for each submenu, so the KDE host re-fetches the layout before showing it, which might fill the menu before Qt's guard fires. It also triggers `LayoutUpdated`, with its own side effects. Untested.

## To check on a newer Plasma

Start the tray (`tisvlay service tray`, or the user service), open a TV's submenu, then hover a nested one (Profile or Input). If it stays open on the first try, the upstream fix has landed: note the Plasma version here and move this entry to Closed.
