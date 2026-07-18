# Post-remote return-wake (`on_local_return`)

## Context

Remote-session awareness (PR #110, merged) blanks a TV when a remote session arms it (`on_remote_connect` → `screen_off`) and leaves it dark when the session disarms (`on_remote_disconnect` → `screen_off`). The floor's stated recovery path (`config.rs` user floor) is that "a returning person wakes it through the now un-armed unlock/lock-wake/idle-resume path." That path has a hole surfaced in QA:

- **unlock / lock-wake** only fire if the desktop was *locked* (`idle_dispatch` gates lock-wake on `is_locked`); a user who didn't lock before disconnecting gets neither.
- **idle-resume** only fires *after a completed unlocked idle cycle* — and the floor default `unlocked_idle_timeout_seconds = 0` means the unlocked idle notification is never even registered (`recompute_idle`/`push_idle` skip `0`), so `IdleResume` cannot fire at all on a stock profile.
- **world DPMS-on** needs a DPMS-off→on *edge*; in the "unlocked, not-yet-idle" window the local output was never DPMS-off, so no edge.

Result: after a remote disconnect, if the user didn't lock and the display hasn't idled/DPMS-off'd, **no event exists** to re-converge the TV (left dark by the disconnect blank) with the still-active local display. Walking back and jiggling the mouse does nothing.

**Fix:** add a first-class, configurable `on_local_return` event that fires on the first genuine local input after a remote disconnect. Floor default: `screen_on` **restoring input** (the TV's input may have drifted while it was blanked and the room's occupant used it for something else). It closes the gap out-of-box while staying fully policy-driven (a profile can set it to `none` to opt out). Detection reuses the always-on 200 ms lock-wake idle probe — no new Wayland registration, no daemon/world changes.

## Detection mechanism

The idle adapter already registers a 200 ms lock-wake notification unconditionally (`LOCK_WAKE_TAG`). While unlocked its `Resumed` edges are currently dropped (`lock_wake_policy` fires only when locked). We reuse them:

1. On `RemoteDisconnect`, the core sets a shared `local_return_armed` flag (an `Arc<AtomicBool>`, mirroring `is_locked` — written by the core's `dispatch`, read by the idle-adapter task).
2. Injected input stops at disconnect → within ~200 ms the seat idles (`LOCK_WAKE_TAG` `Idled`, ignored) → the user's first real input on return produces `LOCK_WAKE_TAG` `Resumed`.
3. `idle_dispatch`, seeing `Resumed` while **unlocked AND `local_return_armed`**, emits `SessionEvent::LocalReturn`.
4. The flag **clears on the next dispatched event** (`LocalReturn` itself, a lock, an idle-timeout, or a new remote connect), so the normal paths own everything afterward. There is no per-cycle latch like lock-wake's `lock_wake_sent`; a burst of `Resumed` before the self-dispatch clears the flag could enqueue a second `LocalReturn`, but `screen_on` is idempotent and the core processes serially, so a single wake results (comments say "clears on the next dispatched event," not "fires exactly once").

### Why `LocalReturn` stays gated

`LocalReturn` is deliberately **left out of `is_remote_edge`**, so it flows through the normal `action_for` `while_remote` gate. This gate is **load-bearing, not inert**: the armed set can move from empty back to non-empty *without* a `RemoteConnect` edge and without clearing the flag — `recompute_remote_armed()` re-runs on profile `update`/`assign`/`create`/`delete` and on `apply_meta`, none of which dispatch `RemoteConnect`. So a return can coincide with a TV that re-armed via a recompute while `local_return_armed` was still set. Because `LocalReturn` is gated and its floor resolves `while_remote = false`, the wake is correctly **suppressed per-TV** for any such armed TV — exactly like a wake for an empty room. The comment on `is_remote_edge` (and the GUI toggle exclusion) states *this* reason — not "the TV is always un-armed when it fires," which is false.

### Known limitations (documented, not fixed in v1)

- **Continuous-input miss:** if the user is inputting *continuously* across the disconnect (no ~200 ms gap for the seat to idle), no `Resumed` edge arms. The effective miss window is as wide as the *disconnect-detection latency* (up to ~2 s for the Sunshine poll), not just the instant of drop: a person who takes over locally during that lag and keeps using the machine only gets their wake when they next pause ≥200 ms and move. Inherent — at disconnect we cannot distinguish present-vs-absent, so we require a *fresh* activity edge as the presence proof.
- **Seat activity ≠ human presence:** the probe tracks the `wl_seat`, which the remote stack also injects into. A residual/teardown input blip arriving >200 ms after the last real input (so the seat idled first) could fire `LocalReturn` for an empty room. Low-likelihood (teardown usually lands within the 200 ms idle window, merely resetting the timer), the wake is idempotent, and it's the mirror of the connect-suppression assumption. If field soaking shows spurious wakes, the mitigation is a short arm-debounce (ignore `Resumed` for ~1–2 s after arming — a real return lands seconds later; teardown noise lands immediately). Deferred to field observation, consistent with the project's "measure before adding knobs" discipline.
- **Lost on restart:** `local_return_armed` is in-memory; a session restart inside the post-disconnect window loses the watch (rare; the next lock/idle or a DPMS edge still recovers eventually). Not persisted — consistent with the other transient session flags. It also has no upper time bound (stays armed until an event clears it or the user returns).
- **Detection synthesis is not covered end-to-end by an automated test** — `idle_dispatch` is unit-tested and the event seam is integration-tested, but the adapter turning a real 200 ms `Resumed` into `LocalReturn` under real timing is only exercised by the manual verification below.

## Implementation

Session-local change — no daemon/world edits (the wake dispatches as an ordinary `set-screen {on:true, restore_input:true}` TV rpc, which the world already honors).

### `crates/tisvlay-core/src/config.rs` — one new event field (**4 hand edits**; `full_default()`/`merge_with_base()`/`resolve()`/`USER_PROFILE_FIELDS` are macro-generated, no manual edit)

- `UserProfileConfig`: add `on_local_return: Option<EventAction>` after `on_remote_disconnect`, before `remote_screencast_arms`, same `#[serde(default, skip_serializing_if = "Option::is_none")]` attr + doc.
- `profile_plumbing! { fields: [...] }`: add `on_local_return,` after `on_remote_disconnect,`.
- `ResolvedUserProfile`: add `pub on_local_return: EventAction` after `on_remote_disconnect`.
- `ResolvedUserProfile::default()`: `on_local_return: EventAction::restoring(SessionAction::ScreenOn)` (`action=ScreenOn`, `restore_input=true`, `while_remote=false`). **Reconcile the floor comment** — it currently says only unlock restores input; reword so unlock *and* local-return restore (a user is (re)arriving at the panel), the other mappings stay input-neutral. Keep the comment about *policy* (why restoring `screen_on` is the floor); do **not** restate the 200 ms-probe detection mechanism here (that lives at `idle_dispatch` and in `detection.md`).

### `crates/tisvlay-session/src/lib.rs`

- `SessionEvent`: add `LocalReturn` variant + doc ("genuine local activity after a remote disconnect — the panel the disconnect left dark comes back for the returning person").
- `tap_name` (exhaustive — arm required): `Self::LocalReturn => "local_return"`.
- `is_remote_edge`: **unchanged** — `LocalReturn` is not an arming edge; it stays gated (see "Why LocalReturn stays gated"; word the comment accordingly).
- `raw_action_for` (exhaustive — arm required): `SessionEvent::LocalReturn => resolved.on_local_return`.
- Mint `let local_return_armed = Arc::new(AtomicBool::new(false));` beside `is_locked`. Clone into the `Core` literal; **move** (last use, no clone) into the `SessionStack` literal.
- `Core` struct: add `local_return_armed: Arc<AtomicBool>` after `is_locked`.
- `SessionStack`: add `pub local_return_armed: Arc<AtomicBool>` after `pub is_locked`.
- `dispatch()`: one-liner near the idle-drift bookkeeping — `self.local_return_armed.store(event == SessionEvent::RemoteDisconnect, Ordering::SeqCst);` (`Ordering`/`AtomicBool` already imported; `SessionEvent: Copy+Eq`). Arms on disconnect, disarms on every other event.
- `idle_adapter`: add a `local_return_armed: Arc<AtomicBool>` param; load it in the select loop and pass to `idle_dispatch`. **Amend the adapter doc-comment** to name the third role of the always-on 200 ms probe: lock-wake when locked, inert when unlocked+unarmed, `LocalReturn` when unlocked+armed.
- `idle_dispatch`: add a `return_armed: bool` param (`IdleTransition: Copy+Eq`, so `transition == Resumed` after the `lock_wake_policy` move is fine). In the `LOCK_WAKE_TAG` arm, after the existing lock-wake decision, if it didn't fire and `!is_locked && return_armed && transition == Resumed` → `Some(SessionEvent::LocalReturn)`. Update the 10 existing unit-test call sites to pass `false`.
- **`resolved_json` (the `resolve` rpc projection):** add `"on_local_return": action(resolved.on_local_return),` beside the other `on_remote_*` keys. Without it the resolve rpc silently omits the field and the GUI/CLI "resolved/inherited" view for that row breaks (no compile error — the `json!{}` is hand-built).

### `crates/tisvlay-session/src/main.rs`

- Pass `stack.local_return_armed.clone()` into the `idle_adapter(...)` call, alongside `is_locked`.

### `crates/tisvlay-gui/src/profiles.rs` — one new field row (**user-side functions only**)

The system-side siblings (`read_system_action`, `read_system_field`, `set_system_field`) must **not** be touched — `SystemProfileBody` has no `on_local_return` and editing them won't compile. Edit only the user-side:

1. `USER_FIELDS`: add a `FieldSpec { wire: "on_local_return", .. }` row after `on_remote_disconnect`.
2. `field_label`: `"on_local_return" => pgettext("profile field", "On local return")` (has a `_` wildcard fall-through — a missing arm is not a compile error but is functionally required).
3. `read_user_action`: `"on_local_return" => body.on_local_return,`.
4. `read_user_field` token getter: add the `on_local_return` arm.
5. `set_user_field` patch match: `"on_local_return" => body.on_local_return = action_field_patch(patch, "on_local_return")?,`.
6. `remote_gate_is_enabled` while-remote-toggle exclusion: add `| "on_local_return"`, and **amend the fn doc** — its current rationale ("those fire at the arming edge where their gate would swallow them") is false for `on_local_return`; give the distinct reason (it's not an arming edge; hiding the toggle is a UI simplification because a `while_remote` toggle on it is a no-op in the common case where the TV is un-armed on return). Note the deliberate backend/GUI asymmetry in one line: connect/disconnect bypass the gate in *both* places; `local_return` keeps the backend gate but hides the GUI toggle.

Getters 3–5 have `_ => None` / `bail!` fall-throughs (silent runtime break if forgotten, not a compile error — add all three). The parity test `user_field_table_matches_core_schema` is auto-satisfied by edit #1. Then `just i18n-extract` and commit the refreshed `.pot`.

### Docs

- `docs/remote/detection.md`: rewrite the opening paragraph (currently asserts the recovery is "the unlock/lock-wake/idle-resume path," the exact gap this closes) and touch the closing "Behavior policy" paragraph — the disconnect blank is now recovered by `on_local_return` (first local input after disconnect). Note the two limitations (continuous-input miss ≈ detection latency; seat-activity-not-presence).
- `docs/ideas/remote-session-followups.md`: add `on_local_return` to the enumerated shipped event set (goes stale otherwise).
- `config.rs` floor comment: fold in the return path (policy only).
- `docs/architecture.md`: no edit — the `event/session/<name>` key entry is generic, not an enumerated list; the eager-refresh docs stay accurate (`local_return` never triggers eager refresh — it fires only when no source is live).

## Testing

- **`idle_dispatch` unit tests** (new `return_armed` param): unlocked+armed+`Resumed` → `LocalReturn`; unlocked+armed+`Idled` → `None`; unlocked+**un**armed+`Resumed` → `None`; locked+armed+`Resumed` → `LockWake` (lock-wake still wins).
- **config**: floor resolves `on_local_return == ScreenOn`, `.restore_input() == true`, `.while_remote() == false`; a profile override to `none` / `power_on` resolves through; merge-with-base test.
- **`resolve` rpc**: the exact-match resolved-defaults assertion in `tests/session.rs` **will fail** once `resolved_json` gains the key — add the expected block `"on_local_return": { "action": "screen_on", "restore_input": true, "while_remote": false }` (mirrors `on_session_unlock`).
- **session `Rig` integration** (`tests/session.rs`, `multi_thread`): report a source → `RemoteConnect` blanks the mock TV; clear it → `RemoteDisconnect` blanks + `stack.local_return_armed` reads `true`; send `SessionEvent::LocalReturn` via the event seam → assert a `set-screen {on:true}` rpc lands + `local_return_armed` reads `false`. Single-drain combined predicates per the zenoh ordering rule.

## Verification (end-to-end, user-assisted)

On the real desktop, TV automated under the default profile: start a Sunshine/Moonlight (or RustDesk) session → TV blanks (`remote_connect`); disconnect → TV stays dark (`remote_disconnect`) **without** locking or waiting out any idle timeout; walk back and move the mouse → TV wakes within ~200 ms (`local_return`, input restored to this PC). Correlate against `journalctl -f --user-unit tisvlay-session` (the `local_return` tap + the `set-screen` dispatch) and `tisvlay --socket <user-sock> watch`. Confirm stock (no-remote) idle/lock/unlock behavior is unchanged, and that disconnect-then-leave (no return) leaves the TV dark until the next real signal. Watch specifically for any spurious wake right at disconnect (the teardown-blip case) — if it recurs, file the arm-debounce follow-up.

## Notes

- No backwards-compat shims (project policy): the field is additive with a neutral `None` default; old stored profiles resolve through the floor to the wake.
- The shared-flag threading mirrors the sole existing precedent (`is_locked`), so it's idiomatic here. If a *third* such adapter/core flag ever appears, bundle the idle-relevant flags into one small shared struct rather than growing the `idle_dispatch` signature further — not worth refactoring for just this second flag.
- On merge, migrate the durable parts per `docs/plans/README.md` (limitations → `docs/concerns/` or `detection.md`; any deferred arm-debounce → `docs/ideas/`) and delete this file.
