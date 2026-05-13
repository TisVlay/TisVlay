# GUI re-activation pulse-attention via `slint::unstable-winit-030`

> **Status:** drafted 2026-05-12, not started. Targets a follow-up PR to PR #54 (`feat/gui-tray-link`). Closes finding F2 from [`gui-desktop-integration.md`](gui-desktop-integration.md) and the related "Raise-on-Activate from the tray is a deliberate no-op" limitation.

## Context

PR #54 ships GUI desktop integration with one substantive UX gap: when the tray's "Open Settings" is clicked (or any launcher re-fires) while the GUI is already running, the `org.freedesktop.Application.Activate` handler is a deliberate no-op (F2 — `slint::Window::show()` on an already-mapped Wayland surface destroys it and exits the event loop, per [slint-ui/slint#4382](https://github.com/slint-ui/slint/issues/4382)). Users have to alt-tab or use the taskbar to find the running window.

A post-merge audit (see PR #54 conversation, 2026-05-12) found the canonical answer to that gap: **`winit::Window::request_user_attention(Some(UserAttentionType::Informational))`** called from inside the GUI process. winit's Wayland backend implements it via an `xdg-activation-v1` token with `XdgActivationTokenData::Attention(...)` and `set_surface(&own_surface)`. KWin maps `Attention`-tagged tokens to `demands_attention` window state — the taskbar entry pulses (and on Low / Medium focus-stealing-prevention, KWin may raise directly). This is exactly the UX that closes the F2 gap.

The same audit found a second, independent win: most of the tray-side `xdg-activation-v1` token-acquisition helper (`activation_token.rs`, ~150 LOC) is **dead weight**. The KIO launcher source ([`kio/src/gui/dbusactivationrunner.cpp:70-96`](../../../libs/kio/src/gui/dbusactivationrunner.cpp#L70-L96)) shows it emits `processStarted` on Activate-reply success regardless of token contents — the tray's empty / surfaceless token never observably contributes anything. Deleting the helper removes ~180 LOC and two crate-level Wayland deps without changing behavior.

Both wins gate on adopting Slint's **`unstable-winit-030`** feature (the `winit_030` re-export that provides `WinitWindowAccessor::with_winit_window`). That's the central design commitment of this PR.

## Goals

1. **Tray-click on running GUI pulses the taskbar entry** (and, on permissive FSP, raises directly). Closes F2.
2. **−150 LOC net** by deleting the now-redundant tray-side `xdg-activation-v1` helper, swapping `Mutex<Option<oneshot::Sender<String>>>` for an idiomatic Tokio primitive, and dropping the `unsafe { std::env::set_var(...) }` block (made obsolete by the new code path).
3. **Three cleanup-table rows retired** in `gui-desktop-integration.md` (50ms timeout tuning, `WindowAttributes`-vs-env-var watch, `unsafe-set_var` cosmetic) — all subsumed.

## Design decisions

### DR1: Adopt `slint::unstable-winit-030`

The feature gates the `slint::winit_030::WinitWindowAccessor::with_winit_window(|w: &winit::window::Window| ...)` accessor that lets us call winit APIs synchronously from inside Slint callbacks. Slint warns the module "may be removed or changed in future minor releases" ([`slint-1.16.1/lib.rs:680-743`](../../../libs/slint/api/rs/slint/lib.rs#L680-L743)).

**Mitigation:** pin `slint = "=1.16.1"` (exact-version) in `crates/tisvlay-gui/Cargo.toml`, plus a comment block explaining the lock. Slint minor-version bumps then become explicit "audit `winit_030` API surface" tasks rather than silent breakage. Workspace-wide Slint upgrades (currently any 1.16.x) become two-step: bump pin, verify `winit_030` still exists and matches.

**Alternative rejected:** direct `winit = "0.30"` dev-dep pinned to the same patch Slint vendors. Skirts the unstable-feature warning but doesn't expose `WinitWindowAccessor` (Slint's accessor is what wraps `with_winit_window`); we'd need to reach the underlying `winit::Window` some other way, which we don't have an entry point for. Hits the same Slint-version-lock requirement anyway.

### DR2: `UserAttentionType::Informational`, not `Critical`

`request_user_attention(Some(UserAttentionType::Informational))` is the right level for "your app is open in the background." `Critical` is for unrecoverable errors and unread-message-like urgency; using it for a launcher re-fire would be screen-pollution. Sees taskbar pulse on KWin Extreme FSP; sees direct raise on Low / Medium FSP.

### DR3: Cold-spawn vs re-activation paths

The new architecture splits handler behavior by window-existence state:

- **Cold-spawn case** (Activate arrives before `AppWindow::new()`): the window doesn't exist yet, so `with_winit_window` would no-op. Instead, the handler stashes the activation-token in `Arc<Mutex<Option<String>>>` and a `BackendSelector::with_winit_window_attributes_hook` reads it at window-construction time, applying it via `WindowAttributesExtWayland::with_activation_token`. This is the architecturally clean path the previous research-audit identified — winit explicitly sends the `xdg_activation_v1.activate` request on our own surface during map. **No env-var manipulation, no `unsafe` block.**
- **Re-activation case** (Activate arrives while `AppWindow` is alive): handler reads the `slint::Weak<AppWindow>` (stored after `AppWindow::new()` via `OnceLock`), calls `slint::invoke_from_event_loop(|| weak.upgrade()?.window().with_winit_window(|w| w.request_user_attention(Some(Informational))))`. Surface exists, F2's `show()`-kill-on-mapped doesn't apply (we're not calling `show()`, we're requesting attention which is a separate Wayland event).

Both paths require `unstable-winit-030`. Same feature, two consumers.

### DR4: Delete `crates/tisvlay-tray/src/activation_token.rs`

Empirical evidence from F4 + the KIO source shows:

1. KIO calls `processEnvironment().value("XDG_ACTIVATION_TOKEN")` which returns empty string when unset; KIO always passes the `activation-token` key in `platform_data`. Empty token is the *expected* normal case for many launchers.
2. KIO's `DBusPendingCallWatcher::finished` emits `processStarted` on Activate-reply success **regardless of token contents** — the reply path is what stops the "Remote peer disconnected" notification.
3. The tray is surfaceless (icon rendered by panel). `xdg_activation_v1.get_activation_token` without `set_surface()` is denied by KWin's policy ~always. We empirically observed empty / failed tokens in `journalctl`.

So the tray's ~150-LOC token helper contributes nothing observable. `activate_gui` should always pass an empty `HashMap<String, Value<'_>>` for `platform_data`. The two crate-level deps `wayland-client` and `wayland-protocols` drop from `crates/tisvlay-tray/Cargo.toml`.

**Side effect**: with the helper gone, `OpenGui` dispatch goes from ~500ms (token blocking) to ~10ms (D-Bus round-trip only). F3's `Arc<AtomicBool>` coalescer was added because the 500ms latency widened the close-mid-queue race window. With the latency gone, the rationale for the coalescer is weak — but the AtomicBool is 15 LOC of defense-in-depth and the audit verdict was "fine as-is, the Tokio-canonical sync/async-split idiom." **Keep the coalescer.** Note the rationale shift in tray.rs.

### DR5: Replace `Mutex<Option<oneshot::Sender<String>>>` with `Arc<Mutex<Option<String>>>` directly

The current `oneshot::Sender` was chosen for typed single-delivery semantics. With the cold-spawn path now reading from a hook (sync, not async), the `oneshot::Receiver` half goes away — we just need `&self.token.lock().take()` from the hook's closure. `Arc<Mutex<Option<String>>>` is simpler and gets rid of the tokio dependency for this particular wire.

For the re-activation path, the handler doesn't need a sender at all — it directly invokes `slint::invoke_from_event_loop` with the captured weak reference.

Net: drop the `tokio::sync::oneshot` import, drop the `oneshot::Receiver` plumbing from `lib.rs`, drop the 50ms timeout in `block_on(timeout(...))`. The `Arc<Mutex<Option<String>>>` lives in `ApplicationService` alongside an `OnceLock<slint::Weak<AppWindow>>` for the re-activation path.

### DR6: `Open` and `ActivateAction` continue to delegate to `raise()`

No change. Spec-required, three-line each, factored through the same `extract_activation_token` + `raise()` path. Audit verdict: "Fine as-is until a 4th method appears." Skip.

### DR7: Plan-doc update + cleanup-table reconciliation

Three rows in [`gui-desktop-integration.md`'s cleanup-table](gui-desktop-integration.md#cleanup-follow-ups) become DONE/obsoleted:

- 50 ms timeout tuning → **DONE** (timeout removed; new path is synchronous through the hook).
- `xdg-activation-v1` plumbing via `WindowAttributes` → **DONE** (this PR is exactly the migration the row described).
- Replace `unsafe { set_var }` → **DONE** (subsumed; no more env-var write at all).

F2 in the findings section gets a "**Resolved by [gui-pulse-on-reactivate.md](gui-pulse-on-reactivate.md)**" note.

## Implementation order

### Commit 1 — Adopt `unstable-winit-030` + ship pulse-attention on re-activation

Files:

- **`crates/tisvlay-gui/Cargo.toml`** — `slint = { workspace = true, features = ["…", "unstable-winit-030"] }` and pin to `=1.16.1` at the workspace level. Plus a comment block citing this plan + DR1.
- **`crates/tisvlay-gui/src/dbus.rs`**:
  - `ApplicationService` gains `Arc<Mutex<Option<String>>>` for cold-spawn token + `OnceLock<slint::Weak<AppWindow>>` for the window handle.
  - `try_claim_and_serve` returns `(ClaimResult, Arc<Mutex<Option<String>>>, OnceLock<slint::Weak<AppWindow>>)` — or wraps them in a small `Activation` struct.
  - `raise()` gains a body: if `window.get().is_some()`, post to Slint event loop and call `with_winit_window(|w| w.request_user_attention(Some(Informational)))`. Otherwise (cold-spawn before AppWindow::new), the token is stashed in the Mutex (existing behavior, just renamed plumbing).
- **`crates/tisvlay-gui/src/lib.rs`**:
  - Build `BackendSelector::new().with_winit_window_attributes_hook(|attrs| …read token from Mutex, apply via with_activation_token… ).select()?`.
  - After `AppWindow::new()`, store `app.as_weak()` into the `OnceLock` so re-activations have a target.
  - Delete: the 50ms `block_on(timeout(...))`, the `unsafe { std::env::set_var(...) }` block, the matching SAFETY comment.

Verification:

- Cold-spawn from KDE app menu → journal shows `Application call received method=Activate token_present=true`. **NEW**: window mapping fires KWin's xdg-activation-v1 activate request explicitly (visible in `wayland-info` / KWin trace if needed).
- Re-activation: launch GUI, alt-tab to another window, click tray's "Open Settings". Journal shows `Application call received`. **NEW**: taskbar entry pulses (KWin Extreme FSP) or window raises directly (Low/Medium FSP). No process death (F2 no longer applies).
- No `Did not receive a reply` dialog (F1 fix still in effect).
- No `Remote peer disconnected` notification on close.

### Commit 2 — Delete tray-side `xdg-activation-v1` helper

Files:

- **`crates/tisvlay-tray/src/activation_token.rs`** — delete.
- **`crates/tisvlay-tray/src/main.rs`**:
  - Drop `mod activation_token;`.
  - Drop the `spawn_blocking(activation_token::try_acquire)` call.
  - `activate_gui` always passes empty `HashMap<String, Value<'_>>` for `platform_data`.
- **`crates/tisvlay-tray/Cargo.toml`** — drop `wayland-client` and `wayland-protocols`.
- **`Cargo.lock`** — regenerate.
- **`crates/tisvlay-tray/src/tray.rs`** — add a doc-comment near the `Arc<AtomicBool>` field noting that F3's rationale shifted: dispatch is now ~10ms / click (D-Bus round-trip only), so multi-spawn pressure dropped. Coalescer kept for defense-in-depth.

Verification:

- `cargo build -p tisvlay-tray` succeeds; the two Wayland deps no longer appear in `cargo tree -p tisvlay-tray`.
- Tray-click flow still works end-to-end: cold spawn from tray click → window appears; re-click on running GUI → taskbar pulse (per commit 1).
- 5 rapid `gdbus call --method com.canonical.dbusmenu.Event` in a burst still spawns exactly 1 GUI.

### Commit 3 — Plan-doc reconciliation

- **`docs/plans/gui-desktop-integration.md`**:
  - F2 paragraph: prepend "**Resolved by [gui-pulse-on-reactivate.md](gui-pulse-on-reactivate.md)**".
  - Cleanup-table: mark three rows as DONE (50ms timeout, WindowAttributes-vs-env-var, unsafe-set_var) with strikethrough + pointer to this plan.
  - Open Questions: "Slint window raise / focus API" gets a partial-resolution note (the pulse-attention path covers the user-experience need; the literal "raise to front" still depends on KWin FSP policy, but that's compositor configuration not a Slint gap).

### Out of scope

- **Direct window-raise** independent of KWin policy. The pulse-attention path is the spec-compliant Wayland mechanism; bypassing FSP would require either a privileged Wayland protocol (`plasma-window-management`, restricted to panels) or a desktop-environment-specific D-Bus method that doesn't exist on KDE. Both rejected during the audit.
- **Auto-restart on unstable-winit-030 breakage.** If a future Slint patch removes `winit_030`, the build breaks. Mitigation is the `=1.16.1` exact-version pin; a future plan covers the migration when we bump Slint.

## Open questions

- **`with_activation_token` for cold-spawn token consumption.** The cleanest implementation routes the captured token through `BackendSelector::with_winit_window_attributes_hook` + `WindowAttributesExtWayland::with_activation_token`. Verify the exact import path during implementation — `slint::winit_030::winit::platform::wayland::WindowAttributesExtWayland` is the expected re-export, but the trait might require feature-gating in winit too.
- **Pulse persistence across user-clicks.** KWin's default behavior is to clear `demands_attention` once the user focuses the window. Verify that repeated tray clicks (re-fires of Activate) don't escalate to flicker / over-pulse. If they do, debounce in the handler (e.g., skip the re-attention call if last one was <500ms ago).
- **GNOME Shell behavior.** Our primary target is KDE Plasma 6, but the same code path should produce GNOME's "running app indicator" pulse via the same xdg-activation-v1 mechanism. Validate on a GNOME session if available, otherwise leave as a "should work" hypothesis.
