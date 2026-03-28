# Review: Transport-Agnostic Wake & Daemon Lifecycle (0da69b0..d010cca)

Review of 5 commits covering two major changes:
1. Transport-agnostic wake refactor (backends own retry strategy)
2. Uniform daemon lifecycle config with reboot detection and boot marker

Build: clean. Tests: all pass.

---

## L1. Boot marker uses synchronous filesystem I/O in async context

**File:** [crates/tisvlayd/src/tv_manager.rs:298-310](crates/tisvlayd/src/tv_manager.rs#L298-L310)

```rust
pub fn on_startup(self: &Arc<Self>) {
    const BOOT_MARKER: &str = "/run/tisvlay/booted";
    let marker_path = std::path::Path::new(BOOT_MARKER);
    if marker_path.exists() {
        info!("Skipping on_startup (service restart, not system boot)");
        return;
    }
    if let Some(dir) = marker_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(marker_path, "");
```

`on_startup` is called from `main()` on the tokio runtime. The `exists()`,
`create_dir_all()`, and `write()` calls are synchronous filesystem I/O. On `/run`
(tmpfs), these are fast enough to be negligible in practice, but this is the same
category of issue flagged in F1/H1 for the config file. The difference is that
`/run` is always tmpfs and never involves real disk I/O, so this is defensible.

However, there's a correctness argument for keeping this synchronous: the marker
check + create must be atomic with respect to the `tokio::spawn` that follows.
If this were `async`, another task could theoretically interleave between the
check and the create (though in practice `on_startup` is only called once from
`main`). The synchronous approach is actually the right call here.

**Severity:** Info (synchronous is correct for this use case)

---

## L2. Boot marker silently ignores write failures

**File:** [crates/tisvlayd/src/tv_manager.rs:307-309](crates/tisvlayd/src/tv_manager.rs#L307-L309)

```rust
let _ = std::fs::create_dir_all(dir);
let _ = std::fs::write(marker_path, "");
```

Both errors are silently discarded with `let _`. If the write fails (e.g.,
`/run/tisvlay/` doesn't exist and `create_dir_all` also failed, or permissions
are wrong), the boot marker is never created and `on_startup` will fire on every
daemon restart — exactly the behavior the marker is supposed to prevent.

The commit message acknowledges this for `--session` dev mode where `/run/tisvlay/`
is not writable, and calls it "desired dev behavior." But in production, a
permissions error would be a silent failure that's hard to diagnose. A `warn!`
on failure would make the tradeoff explicit:

```rust
if let Err(e) = std::fs::write(marker_path, "") {
    warn!(error = %e, "Failed to write boot marker — on_startup will fire on every restart");
}
```

**Severity:** Low (silent failure in production is a diagnostic blind spot)

---

## L3. `ScheduledShutdown` fallback to `DaemonAction::Shutdown` on error

**File:** [crates/tisvlayd/src/logind.rs:190-201](crates/tisvlayd/src/logind.rs#L190-L201)

```rust
let (shutdown_type, daemon_action) = match proxy.scheduled_shutdown().await {
    Ok((type_str, _usec)) => {
        let action = match type_str.as_str() {
            "reboot" | "kexec" => DaemonAction::Reboot,
            _ => DaemonAction::Shutdown,
        };
        (type_str, action)
    }
    Err(e) => {
        warn!(error = %e, "Failed to query ScheduledShutdown, assuming poweroff");
        ("poweroff".to_string(), DaemonAction::Shutdown)
    }
};
```

Falling back to `Shutdown` on error is the safe default — if we can't determine
the shutdown type, powering off TVs is the conservative choice (a reboot that
unexpectedly powers off TVs is inconvenient; a poweroff that leaves TVs on wastes
power and risks burn-in).

One edge case: `ScheduledShutdown` returns `("", 0)` when no shutdown is
scheduled. If `PrepareForShutdown(true)` fires but `ScheduledShutdown` returns
an empty string, the `_` arm maps it to `DaemonAction::Shutdown`. This is
correct — if systemd is telling us to prepare for shutdown but hasn't set a type,
`Shutdown` is the only safe assumption.

**Severity:** Info (correct fallback behavior)

---

## L4. `wake_and_connect` trait method now has no default implementation

**File:** [crates/tisvlay-core/src/traits.rs:262-268](crates/tisvlay-core/src/traits.rs#L262-L268)

```rust
async fn wake_and_connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>>;
```

The old trait provided a default impl (wake, sleep `boot_delay`, retry connect
N times). The new trait is a bare required method — backends must implement it.

This is the right call. The default impl was misleading: it assumed all transports
need a WoL-then-TCP-connect pattern, which is only true for network-based backends.
A USB or RS232 backend would have a completely different wake strategy. Making it
required forces each backend to think about its own transport characteristics.

The LG backend's implementation (hardcoded `WAKE_CONNECT_MAX_ATTEMPTS=10`,
`WAKE_CONNECT_RETRY_INTERVAL=3s`) is correct for WebOS TVs. The constants are
documented and the retry logic matches the old behavior minus the unnecessary
initial `boot_delay` sleep — the WebSocket connection timeout provides natural
pacing.

**Severity:** Info (correct design decision)

---

## L5. Hardcoded constants in LG backend vs. configurable timing

**File:** [crates/tisvlay-lg/src/backend.rs:17-21](crates/tisvlay-lg/src/backend.rs#L17-L21)

```rust
const WAKE_CONNECT_MAX_ATTEMPTS: u32 = 10;
const WAKE_CONNECT_RETRY_INTERVAL: Duration = Duration::from_secs(3);
```

Previously these were configurable via `wake_delay_ms` in the daemon config. Now
they're hardcoded in the LG backend. This is intentional — the commit message
explains that timing is "fundamentally transport-specific."

For LG WebOS TVs, 10 attempts x 3 seconds = ~30 seconds max wait, which matches
the observed 5-15 second boot time with margin. The values are reasonable.

The tradeoff: if a user has a TV that takes longer to boot (e.g., older firmware),
they have no config knob to adjust. This is acceptable for now — if it becomes a
real-world issue, the LG backend can add LG-specific config fields later (e.g.,
in `LgVendorConfig`). No need to pre-engineer this.

**Severity:** Info (acceptable tradeoff; LG-specific config is the right future path)

---

## L6. `execute_action_all_tvs` moved to `TvManager` — good refactor

**File:** [crates/tisvlayd/src/tv_manager.rs:287-304](crates/tisvlayd/src/tv_manager.rs#L287-L304)

Previously a free function in `logind.rs`, now a method on `TvManager`. This is
the right move: `on_startup` also needs it, and `TvManager` is the natural owner
of "do something to all managed TVs." The `logind.rs` call sites now use
`manager.execute_action_all_tvs(action, context)` which is cleaner.

**Severity:** Info (correct refactoring)

---

## L7. Journald log formatting — `with_target(false)` then restored

**Commits:** 4b17db7 strips both timestamp and target; d010cca restores target.

The first commit removed both `without_time()` and `with_target(false)` under
journald. The follow-up commit reverts `with_target(false)`, keeping timestamps
stripped but targets visible. The reasoning in d010cca is sound: journald provides
unit names (e.g., `tisvlayd.service`) but not Rust module paths (e.g.,
`tisvlayd::tv_manager`). The target is genuinely useful for subsystem disambiguation.

The two-commit sequence shows healthy iteration. End state is correct.

**Severity:** Info (correct final state)

---

## L8. Duplicated journald log init across three binaries

**Files:**
- [crates/tisvlayd/src/main.rs:51-65](crates/tisvlayd/src/main.rs#L51-L65)
- [crates/tisvlay-session/src/main.rs:200-217](crates/tisvlay-session/src/main.rs#L200-L217)
- [crates/tisvlay-tray/src/main.rs:39-55](crates/tisvlay-tray/src/main.rs#L39-L55)

All three binaries now have the same `if under_journal { ... } else { ... }` pattern
for configuring tracing. The only differences are the crate-specific filter directives
(e.g., `tisvlayd=info` vs `tisvlay_tray=info`) and the tray's extra `ksni=warn`.

This is a candidate for extraction into a shared helper in `tisvlay-core`, e.g.:

```rust
pub fn init_tracing(crate_filter: &str, extra_filters: &[&str]) { ... }
```

However, the duplication is only 15 lines per binary and the differences are
real (different crate names, tray has an extra filter). Extracting now would
add a dependency from `tisvlay-core` on `tracing-subscriber` (currently only
the binaries depend on it), which may not be desirable.

**Severity:** Low (mild duplication; extraction is optional and has dependency tradeoffs)

---

## L9. `on_startup` default is `power_on` — correct UX default

**File:** [crates/tisvlay-core/src/config.rs:243-245](crates/tisvlay-core/src/config.rs#L243-L245)

```rust
fn default_on_startup() -> SessionAction {
    SessionAction::PowerOn
}
```

The default for `on_startup` is `PowerOn` (wake TVs on system boot). This matches
the most common use case: a desktop connected to TVs as monitors. The boot marker
ensures this only fires on actual system boot, not daemon restarts.

`on_reboot` defaults to `None` — TV stays on during reboot so the user can see
BIOS/TTY output. `on_shutdown` defaults to `PowerOff`. `on_no_session` defaults
to `None`. All four defaults match the expected desktop experience.

**Severity:** Info (correct defaults)

---

## L10. `RuntimeDirectoryPreserve=restart` — correct systemd integration

**Files:**
- [systemd/tisvlayd.service](systemd/tisvlayd.service)
- [nix/module.nix](nix/module.nix)

Both the manual systemd unit and the NixOS module now set `RuntimeDirectory=tisvlay`
and `RuntimeDirectoryPreserve=restart`. This means:
- On `systemctl restart tisvlayd`: `/run/tisvlay/booted` survives, `on_startup` skips
- On system reboot: `/run/` (tmpfs) is cleared, `on_startup` fires

The NixOS module change is minimal (2 lines) and mirrors the systemd unit exactly.

**Severity:** Info (correct integration)

---

## L11. `set_daemon_option` key matching simplified with combined arm

**File:** [crates/tisvlayd/src/tv_manager.rs:943-958](crates/tisvlayd/src/tv_manager.rs#L943-L958)

```rust
"on_startup" | "on_shutdown" | "on_reboot" | "on_no_session" => {
    let action: SessionAction = serde_json::from_value(...)...;
    match key {
        "on_startup" => daemon.on_startup = action,
        "on_shutdown" => daemon.on_shutdown = action,
        "on_reboot" => daemon.on_reboot = action,
        "on_no_session" => daemon.on_no_session = action,
        _ => unreachable!(),
    }
}
```

The old code had separate arms for each key with duplicated `serde_json::from_value`
calls. The new version validates once, then dispatches. This eliminates the duplication
and makes it trivial to add new lifecycle events. The `unreachable!()` is correct
since the outer match already constrains the key.

Also removes `wake_on_boot` and `wake_delay_ms` keys entirely — clean break since
these config fields no longer exist.

**Severity:** Info (good simplification)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| L1 | **Info** | Synchronous boot marker I/O is correct (atomicity, tmpfs) |
| L2 | **Low** | Boot marker silently ignores write failures — add `warn!` |
| L3 | **Info** | `ScheduledShutdown` fallback to Shutdown is safe default |
| L4 | **Info** | No default impl for `wake_and_connect` is correct design |
| L5 | **Info** | Hardcoded LG timing constants are acceptable; future path is LG-specific config |
| L6 | **Info** | `execute_action_all_tvs` correctly moved to `TvManager` |
| L7 | **Info** | Journald target restored after iteration — correct final state |
| L8 | **Low** | Journald init duplicated across 3 binaries (extraction optional) |
| L9 | **Info** | All four lifecycle defaults match desktop UX expectations |
| L10 | **Info** | `RuntimeDirectoryPreserve=restart` correct for boot marker |
| L11 | **Info** | `set_daemon_option` matching simplified, old keys cleanly removed |

## Overall Assessment

This is a well-executed refactor. The transport-agnostic wake change correctly moves
timing policy from the daemon to the backend where it belongs. The lifecycle config
unification (four `SessionAction` fields replacing a mixed bool/u64/action set) is
cleaner and more extensible. The boot marker is a pragmatic solution to the
restart-vs-reboot problem, with correct systemd integration in both the manual unit
and NixOS module.

The only actionable item is **L2**: add a `warn!` when the boot marker write fails,
so production failures are observable. Everything else is info-level confirmation
of correct behavior.
