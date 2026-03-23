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
