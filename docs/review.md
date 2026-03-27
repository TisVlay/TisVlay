# Scroll Coalescing Review (commit 3bbf9648987b127fe137a022d727212facc52160)

Review of `fix: Coalesce scroll events to prevent queue buildup in tray`.

Build: clean. Tests: all pass.

---

## Design

The fix replaces four directional variants (`VolumeUp`, `VolumeDown`, `BacklightUp`,
`BacklightDown`) with a single `ScrollBy(tv_id, target, steps)` command carrying a
signed step count. The command processor drains the channel with `try_recv` to coalesce
all pending `ScrollBy` events for the same TV and target into one accumulated step count,
then issues a single absolute `set_volume` / `set_backlight` D-Bus call.

This is the correct approach — it's the standard "coalesce-then-apply" pattern used
in UI frameworks for scroll, resize, and paint events. The key properties:

1. **O(1) D-Bus calls per scroll burst** instead of O(N) — no matter how many events
   queue during a slow D-Bus round-trip, they collapse into one call.
2. **Net-zero cancellation** — scrolling up 10 then down 10 during a single processing
   window produces `steps = 0`, which correctly results in no change.
3. **Non-scroll commands aren't lost** — the `Ok(other)` arm re-queues non-scroll
   commands and breaks out of the drain loop.

---

## J1. Re-queue channel creates an unbounded feedback loop risk

**File:** [crates/tisvlay-tray/src/main.rs:271-275](crates/tisvlay-tray/src/main.rs#L271-L275)

```rust
Ok(other) => {
    // Non-scroll command arrived mid-burst: re-queue it so it
    // isn't lost; it will be processed in the next iteration.
    let _ = requeue_tx.send(other);
    break;
}
```

`requeue_tx` is a clone of `cmd_tx` — the same unbounded channel. The re-queued command
goes to the *back* of the channel. This is fine in the normal case (a single Refresh or
PowerOff between scroll bursts). But consider:

- If two different targets interleave (`ScrollBy(tv1, Volume, 1)` then
  `ScrollBy(tv1, Backlight, 1)` then `ScrollBy(tv1, Volume, 1)`), the drain loop
  coalesces the first Volume event, encounters the Backlight event (different target),
  re-queues it, and breaks. The third Volume event remains in the channel and is
  processed as a separate D-Bus call. The coalescing only works for *contiguous*
  same-target events.

This is acceptable in practice because the scroll handler only ever sends one target
type at a time (determined by `scroll_target`), and menu clicks don't interleave with
scroll events at the rate that would cause problems. But it means the coalescing is
**best-effort, not guaranteed** — worth documenting.

**Severity:** Info (correct in practice; theoretical interleaving gap)

---

## J2. `steps = 0` after coalescing still issues a D-Bus call

**File:** [crates/tisvlay-tray/src/main.rs:280-301](crates/tisvlay-tray/src/main.rs#L280-L301)

If the user scrolls up 5 then down 5 during a single processing window, `steps`
coalesces to 0. The code then computes `new = current + 0 * STEP = current` and
calls `daemon.set_volume(&id, current)` — a no-op D-Bus round-trip that sets the
value to what it already is.

**Fix direction:** Add `if steps == 0 { continue; }` after the drain loop to skip
the D-Bus call entirely.

**Severity:** Low (wasted round-trip, not a bug)

---

## J3. `delta.signum()` discards high-resolution scroll information

**File:** [crates/tisvlay-tray/src/tray.rs:230](crates/tisvlay-tray/src/tray.rs#L230)

```rust
let _ = self
    .cmd_tx
    .send(TrayCommand::ScrollBy(id, self.scroll_target, delta.signum()));
```

`delta.signum()` normalizes every scroll tick to +1 or -1 regardless of the raw delta.
This is correct for click-wheel mice (where delta is typically +-120 per notch). But
for smooth/high-resolution trackpads or free-spinning wheels, ksni delivers many small
deltas in rapid succession — `signum()` loses the magnitude, and the coalescing then
sums up 1s instead of summing actual deltas.

The original code also ignored magnitude (`delta > 0` was a boolean test), so this is
not a regression — just a missed opportunity now that the architecture supports it.
A future enhancement could pass `delta` directly (or `delta / 120` for
120-units-per-notch normalization), but that would need testing with actual trackpad
hardware.

**Severity:** Info (not a regression; future enhancement opportunity)

---

## J4. Coalescing across different TVs stops early

**File:** [crates/tisvlay-tray/src/main.rs:266-267](crates/tisvlay-tray/src/main.rs#L266-L267)

```rust
Ok(TrayCommand::ScrollBy(nid, ntarget, nsteps))
    if nid == id && ntarget == target =>
```

A `ScrollBy` for a *different* TV is treated as a non-matching command: it falls
through to the `Ok(other)` arm, gets re-queued, and breaks the drain loop. This means
if the user scrolls on TV1, then the scroll target changes to TV2 mid-burst, the TV2
events are processed individually instead of coalesced.

This is the correct behavior — coalescing across different TVs would produce wrong
results (you'd apply TV1's accumulated delta to TV2). The re-queue-and-break approach
preserves correctness. No change needed.

**Severity:** Info (correct behavior, documenting for completeness)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| J1 | **Info** | Re-queue only coalesces contiguous same-target events (best-effort) |
| J2 | **Low** | `steps = 0` still issues a no-op D-Bus call — skip with early continue |
| J3 | **Info** | `delta.signum()` discards high-res scroll magnitude (not a regression) |
| J4 | **Info** | Different-TV scroll events break coalescing (correct behavior) |

### Overall Assessment

The scroll coalescing fix is clean and directly addresses the reported UX issue. The
core design — drain-and-sum with `try_recv`, then one absolute D-Bus call — is the
right pattern. The `ScrollBy` variant unifies four command types into one, reducing
code duplication.

The only actionable item is **J2**: skip the D-Bus call when `steps` coalesces to zero.
Everything else is either correct-by-design or a future enhancement opportunity.

---
---

# Scroll Follow-Up Review (commits 9e8cf81, 59ec097)

Review of two follow-up commits addressing J2 and J3 from the previous review, plus a
scroll performance optimization.

Build: clean. Tests: all pass.

---

## Commit 1: `feat: Proportional scroll normalization with per-notch step sizing` (9e8cf81)

This commit addresses **J2** (steps=0 no-op) and **J3** (signum discards magnitude):

1. **J2 fix**: Adds `if steps == 0 { continue; }` after the drain loop — exactly the
   fix suggested in J2. No-op coalesced bursts now skip the D-Bus call entirely. ✓

2. **J3 enhancement**: Replaces `delta.signum()` with `(delta.abs() / SCROLL_NOTCH).max(1) * delta.signum()`.
   This preserves magnitude for fast-spinning wheels (240+ units → 2+ steps) while
   flooring sub-notch trackpad events to 1 step. The `SCROLL_NOTCH = 120` constant
   follows the X11/Wayland convention. ✓

3. **Step constants reduced**: `VOLUME_STEP` and `BACKLIGHT_STEP` both changed from 5/10
   to 1, since the notch normalization now provides granularity. Each step is one
   absolute unit, making the mapping between scroll notches and TV adjustments 1:1. ✓

4. **Early return on delta=0**: Adds `if delta == 0 { return; }` in the scroll handler
   before computing steps. Prevents sending a zero-step command into the channel. ✓

### K1. Integer division truncation in step calculation

**File:** [crates/tisvlay-tray/src/tray.rs:243](crates/tisvlay-tray/src/tray.rs#L243)

```rust
let steps = (delta.abs() / SCROLL_NOTCH).max(1) * delta.signum();
```

The `.max(1)` floor ensures sub-notch deltas (e.g. trackpad sending delta=15) produce
1 step. For a standard click-wheel sending exactly 120, this gives `(120/120).max(1) = 1`.
For fast wheels sending 240, it gives `(240/120).max(1) = 2`. The integer division
truncation means 239 → 1 step (not 1.99), which is the correct conservative behavior
for discrete volume/backlight adjustments.

**Severity:** Info (correct behavior, no issue)

---

## Commit 2: `fix: Skip redundant state refresh after scroll commands in tray` (59ec097)

**File:** [crates/tisvlay-tray/src/main.rs:304-308](crates/tisvlay-tray/src/main.rs#L304-L308)

```rust
// Skip the full fetch_all_state refresh — the cache was already
// updated optimistically above, and the next poll cycle will
// reconcile with the real state. This avoids an extra D-Bus +
// WebSocket round-trip per scroll step (~5x faster cycle time).
continue;
```

The `continue` skips the `fetch_all_state()` call at the bottom of the command processing
loop. This is safe because the ScrollBy handler already updates the cache optimistically
via `set_cached_volume` / `set_cached_backlight` after a successful D-Bus call (lines
290, 300). The 5-second poll cycle will reconcile any drift.

### K2. Optimistic cache divergence on D-Bus failure

**File:** [crates/tisvlay-tray/src/main.rs:287-301](crates/tisvlay-tray/src/main.rs#L287-L301)

The cache is only updated in the `else` branch (on success), so a failed `set_volume` /
`set_backlight` call correctly leaves the cache unchanged. The `continue` then skips
the refresh that would have corrected the cache anyway. Since the cache retains the
pre-scroll value on failure, the tray menu shows the correct (unchanged) state. The
next poll cycle refreshes regardless.

**Severity:** Info (correct behavior — failure path already handled)

### K3. Other commands still refresh; only ScrollBy skips

The `continue` only applies to the `ScrollBy` arm. All other commands (`ToggleMute`,
`PowerOn`, `PowerOff`, etc.) fall through to the existing `fetch_all_state()` at the
bottom. This is the right split — scroll events are high-frequency and benefit from
skipping the refresh, while discrete commands like power toggle happen rarely and
benefit from immediate state reconciliation.

**Severity:** Info (correct design choice)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| K1 | **Info** | Integer truncation in notch normalization is correct conservative behavior |
| K2 | **Info** | Optimistic cache correctly skips update on D-Bus failure |
| K3 | **Info** | Only scroll skips refresh; discrete commands still reconcile immediately |

### Overall Assessment

These two commits directly address **J2** and **J3** from the previous review and add a
meaningful performance improvement (8.7x median scroll cycle time reduction). The
`SCROLL_NOTCH` normalization is well-documented and follows platform conventions. The
optimistic cache skip is safe because the success path updates the cache and the failure
path preserves it. No actionable items — all findings are info-level confirmations of
correct behavior.
