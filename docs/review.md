# Review: NetworkManager-aware reconnection, Transport enum, CLI/tray UX (main..HEAD)

Review of 33 commits (including experimental variants that were reverted). The net diff
reflects the final settled direction: Variant A+ (per-TV reconnecting flag) with 500ms
WoL retries and Transport enum.

Build: clean. Tests: all pass.

---

## V1. NetworkManager watcher — clean event-driven design

**File:** [crates/tisvlayd/src/network_watcher.rs](crates/tisvlayd/src/network_watcher.rs)

Well-structured module:
- `NmState` enum maps the NM state integer values with `has_network()` helper
- `zbus::proxy` macro for `StateChanged` signal subscription
- Transition tracking: only emits `NetworkRestored`/`NetworkLost` on actual
  has_network edge transitions (not every state change)
- System bus always (NM is system-bus-only), regardless of `--session` flag
- Graceful degradation: logs warning and returns if system bus or NM unavailable

One subtlety handled correctly: `get_nm_state()` uses `inner().get_property::<u32>("State")`
instead of a proxy property method, avoiding a name conflict between the `StateChanged`
signal and the `State` property (both would generate `state()` in the proxy).

**Severity:** Info (correct design)

---

## V2. `reconnecting` flag prevents NM/WoL racing (Variant A+)

**File:** [crates/tisvlayd/src/tv_manager.rs](crates/tisvlayd/src/tv_manager.rs)

The plan documents 5 variants tested with measured benchmarks. The selected approach
(Variant A+) uses a `reconnecting: bool` flag on `ManagedTv`:

- Set `true` in `power_on()` before dropping the stale connection
- Set `false` after `wake_and_connect_tv()` completes (success or failure)
- `reconnect_disconnected()` skips TVs with `reconnecting=true`
- `disconnect_network_tvs()` skips TVs with `reconnecting=true`

This prevents both directions of interference: NM reconnect doesn't race with WoL,
and NM disconnect doesn't kill an in-flight WoL connection.

The flag is cleared in a `match result {}` block that handles both success and error,
so it never gets stuck `true` on failure. Good.

**Severity:** Info (correct race prevention)

---

## V3. Benchmarked variants with measured results — excellent methodology

**File:** [docs/plan.md](docs/plan.md) — test matrix section

The plan documents 8+ test variants with measured timings from real hardware:
- Baseline (no NM), Variant 1 (NM no debounce), Variant 2 (NM + debounce),
  Variant 3B (wake window), Variant A, Variant A+, Variant C (deferred wake)
- D-Bus power cycle (hardware minimum), static IP experiment
- Each variant includes: sleep→disconnect, wake→subscribed, duplicate attempts, notes

The improvement breakdown shows 500ms retries provide the biggest win (3-6s faster),
while the NM watcher primarily prevents racing and adds instant disconnect detection.

The remaining bottleneck analysis (Ethernet link negotiation + TV firmware boot) shows
the daemon is now within ~2s of the theoretical minimum.

**Severity:** Info (thorough engineering methodology)

---

## V4. Transport enum — correct abstraction level

**File:** [crates/tisvlay-core/src/traits.rs](crates/tisvlay-core/src/traits.rs)

```rust
pub enum Transport { Network, Serial, Usb, Cec }
```

`is_network_dependent()` returns `true` only for `Network`. The `TvConnector` trait
gains a `transport()` method. `BackendRegistry::transport_for()` exposes it to the daemon.

`disconnect_network_tvs()` and `reconnect_disconnected()` skip non-network TVs. This
means future serial/USB/CEC backends won't be disrupted by NM events.

Default for unknown vendors is `Network` (safe — NM events will disconnect them, which
is correct behavior for unknown transports on the network).

**Severity:** Info (correct future-proofing)

---

## V5. 500ms WoL retry interval — validated by benchmarks

**File:** [crates/tisvlay-lg/src/backend.rs](crates/tisvlay-lg/src/backend.rs)

```rust
const WAKE_CONNECT_MAX_ATTEMPTS: u32 = 60;
const WAKE_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(500);
```

Changed from 10 attempts × 3s to 60 attempts × 500ms. Each attempt is ~50ms (TCP
connect + WebSocket close frame), so the cost is negligible. The TV is caught within
one cycle of SSAP becoming ready instead of potentially waiting up to 2.9s for the
next 3s retry.

Benchmarks confirm: D-Bus power cycle went from variable to consistent ~6.8s. Sleep
wake improved from 15.8s to 10-13s (the variance is TV firmware boot time, not daemon).

**Severity:** Info (validated improvement)

---

## V6. Two-phase LG pairing registration — correct fix

**Files:**
- [crates/tisvlay-lg/src/client.rs](crates/tisvlay-lg/src/client.rs)
- [crates/tisvlay-lg/src/connection.rs](crates/tisvlay-lg/src/connection.rs)

First-time pairing sends: `register request` → TV shows prompt → `Response` ack →
user accepts → `Registered` with client key. The oneshot-based `request()` only
captured the first response (`Response` ack), causing "missing field client-key".

Fix: `request_wait(id, timeout)` registers a new oneshot for the same request ID
without re-sending. The `register()` method now handles both phases: if the first
response is `Response` (ack), it calls `request_wait()` for the `Registered` phase.
If the first response is already `Registered` (re-pairing), it returns immediately.

**Severity:** Info (correct protocol handling)

---

## V7. CLI improvements — `--json`, `--managed`, enriched status

**File:** [crates/tisvlay-cli/src/main.rs](crates/tisvlay-cli/src/main.rs)

- `--json` global flag for machine-readable output (Nushell-friendly)
- `--managed true/false` replaces `--unmanaged` (more explicit, uses `ArgAction::Set`)
- `tisvlay status` shows full TV info: vendor, address, profiles, inputs with current marker
- `Manage` command shows current state when called without argument

The `print_tv_status` function uses indented tree format matching the tray tooltip style.

**Severity:** Info (good UX improvements)

---

## V8. Tray tooltip tree view with scroll target indicator

**File:** [crates/tisvlay-tray/src/tray.rs](crates/tisvlay-tray/src/tray.rs)

The tooltip now shows an indented tree with `►` marking the active scroll target:

```
Living Room:
► volume 18
  backlight 10
Office:
  volume 50
  backlight 80
```

`secondary_activate` (middle-click) cycles `scroll_tv_id` to the next connected TV.
The cycle wraps around correctly via `(i + 1) % connected.len()`.

**Severity:** Info (good multi-TV UX)

---

## V9. RUST_LOG precedence fix in logging init

**File:** [crates/tisvlay-core/src/logging.rs](crates/tisvlay-core/src/logging.rs)

When `RUST_LOG` is set, hardcoded directives are skipped entirely. This means
`RUST_LOG=debug` in a VS Code task debug config now works as expected instead of
being overridden by `tisvlayd=info`.

The check uses `std::env::var_os("RUST_LOG").is_some()` (presence, not value), so
even `RUST_LOG=""` triggers the override path. This is correct — `EnvFilter::from_default_env()`
handles empty values.

**Severity:** Info (correct fix)

---

## V10. `reconnect_disconnected` holds per-TV locks under outer read lock

**File:** [crates/tisvlayd/src/tv_manager.rs:715-750](crates/tisvlayd/src/tv_manager.rs#L715-L750)

```rust
let tvs = self.tvs.read().await;
for (tv_id, (managed, tv_arc)) in tvs.iter() {
    let tv = tv_arc.lock().await;
    if !self.registry.transport_for(&tv.config).is_network_dependent() { continue; }
    if tv.is_connected() || tv.powered_off || tv.reconnecting { continue; }
    drop(tv);
    tokio::spawn(async move { manager.reconnect(&tv_id).await; });
}
```

The per-TV lock is held briefly (transport check + flag checks), then explicitly dropped
before spawning the reconnect task. The outer read lock is held for the iteration but
doesn't block TV commands (only blocks `pair_tv`/`remove_tv`). The reconnect itself
runs in a spawned task outside both locks.

`disconnect_network_tvs` has a similar pattern but calls `conn.close()` under the per-TV
lock. This is a network operation that could block, but during network loss the close is
typically fast (no TCP handshake needed since the network is down).

**Severity:** Info (acceptable pattern)

---

## V11. `powered_off` flag not cleared by `disconnect_network_tvs`

**File:** [crates/tisvlayd/src/tv_manager.rs:758-785](crates/tisvlayd/src/tv_manager.rs#L758-L785)

`disconnect_network_tvs` drops connections but doesn't set `powered_off = true`. This
is correct — the TV wasn't intentionally powered off, just disconnected. When the
network comes back, `reconnect_disconnected` should attempt to reconnect these TVs
(which it does — it checks `tv.powered_off` and skips only intentionally-off TVs).

If the TV was powered off via `power_off()`, that sets `powered_off = true`, and
`reconnect_disconnected` correctly skips it.

**Severity:** Info (correct flag semantics)

---

## V12. `state_change_forwarder` now takes `manager: Arc<TvManager>`

**File:** [crates/tisvlayd/src/main.rs:261-268](crates/tisvlayd/src/main.rs#L261-L268)

The forwarder needs the manager to call `reconnect_disconnected()` and
`disconnect_network_tvs()` on NM events. Previously it only had the D-Bus connection.
The manager is passed as an `Arc` clone — same pattern as logind and the startup task.

**Severity:** Info (correct wiring)

---

## V13. Alphabetical TV sort in tray

The tray now sorts TVs alphabetically by name. This ensures a stable, predictable
order in the menu and tooltip regardless of config file ordering or pairing sequence.

**Severity:** Info (good UX)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| V1 | **Info** | NM watcher is clean event-driven design with graceful degradation |
| V2 | **Info** | Reconnecting flag prevents NM/WoL racing correctly |
| V3 | **Info** | Benchmark methodology is thorough (8+ variants measured) |
| V4 | **Info** | Transport enum correctly scopes NM events to network backends |
| V5 | **Info** | 500ms WoL retry validated by benchmarks |
| V6 | **Info** | Two-phase LG pairing handles both fresh and re-pair flows |
| V7 | **Info** | CLI improvements (--json, --managed, enriched status) |
| V8 | **Info** | Tray tooltip tree view with scroll target indicator |
| V9 | **Info** | RUST_LOG precedence fix for development workflows |
| V10 | **Info** | Lock patterns in reconnect/disconnect are acceptable |
| V11 | **Info** | powered_off flag semantics are correct |
| V12 | **Info** | Forwarder correctly wired with manager for NM events |
| V13 | **Info** | Alphabetical TV sort in tray |

## Overall Assessment

This branch covers three areas: NetworkManager-aware reconnection (the main feature),
transport abstraction (future-proofing), and CLI/tray UX improvements.

The NM watcher is well-designed: event-driven via D-Bus signals (no polling), graceful
degradation without NM, and scoped to network-dependent backends via the Transport enum.
The racing problem between NM reconnect and WoL was methodically explored (5 variants
benchmarked on real hardware) with the per-TV `reconnecting` flag selected for its clean
semantics.

The 500ms WoL retry provides the biggest measurable improvement (3-6s faster wake),
while the NM watcher adds instant disconnect detection (3ms vs minutes for TCP keepalive)
and sub-second network blip recovery (136ms).

No actionable items. All 13 findings are info-level.
