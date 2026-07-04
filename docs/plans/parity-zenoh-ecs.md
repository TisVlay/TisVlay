# Parity: driving the zenoh+ECS stack to full TisVlay parity

Status: **decisions resolved (2026-07-04) — P1 ready to start** (see Resolved decisions).

## Goal

The spike ([spike-zenoh-ecs.md](spike-zenoh-ecs.md)) exited with a keep verdict: continue the zenoh + bevy_ecs stack to complete feature parity with the zbus stack, so it can be battle-tested end to end — daemon, CLI, session service, greeter, tray, and GUI — as a daily driver. The old stack stays runnable until the new one has survived real duty (switchover criteria at the bottom).

## Architecture decisions

### Two-socket topology and the transitive-routing pitfall

The zbus stack separates a system bus (daemon, policy-gated) from per-user session buses (session service, user profiles). The zenoh approximation needs care because **zenoh peer routing is transitive**: if per-user session peers and the system daemon form one connected mesh, any client anywhere in the mesh can subscribe to any key — user B could read user A's `user/**` keys straight through the system daemon. Separate sockets alone do not reproduce D-Bus's session isolation.

Proposed design — **the session service is an application-level gateway, not a router**:

- **System daemon**: zenoh peer, listener on `/run/tisvlay/zenoh.sock` (0660, group `tisvlay`). Owns `tisvlay/<site>/**` except the `user/` subtree.
- **Per-user session service**: runs **two zenoh sessions**. Session A: client → system socket (consumes TV state, publishes session events on `tisvlay/<site>/event/session/*`, calls rpcs). Session B: peer with a listener on `$XDG_RUNTIME_DIR/tisvlay/session.sock` (0700 runtime dir = per-user privacy for free), owning the `tisvlay/<site>/user/**` subtree (user profiles, per-TV assignments, its own liveliness token). Nothing routes between A and B except explicit application logic — user-private keys physically never reach the system peer.
- **User clients (GUI, tray, CLI)**: **two zenoh client sessions**, one per socket. (The original one-session-two-endpoints idea died in P4: a zenoh *client* maintains a single link — `start_client` connects with `single_link: true`, so a multi-endpoint connect list is failover, not fan-out. Two sessions make the isolation structural anyway: nothing a client does can bridge the key spaces.) Clients route by key: `user/**` on the user session, everything else on the system session; fleet-wide watches run one subscriber per session.
- **Greeter**: client → system socket only (greeter user joins the `tisvlay` group), publishing `tisvlay/<site>/greeter/idle` exactly as the spike does.

Alternatives considered: (a) single connected mesh + zenoh ACL rules denying `user/**` across the system daemon's links — keeps one session everywhere but rests the privacy boundary on `unstable` ACL semantics instead of the filesystem; (b) single system socket, user keys namespaced by uid — no privacy at all between logged-in users. **Decided: the gateway design** (2026-07-04).

Carried risk, now sharper with multi-user in scope: within one socket's group, there is no per-method or per-caller authorization (the greeter user can technically publish anything on the system mesh). This was the accepted regression from the original spike plan; polkit-shaped gating would be a post-parity design if it ever matters.

### Persistence

The daemon gains a `store` layer: a TOML file (`/var/lib/tisvlay/…` in production, `--state-dir` for dev) holding TVs (including vendor configs with credentials), per-TV settings, and system profiles. The world emits a `Persist` effect (full snapshot) whenever config-shaped components change; the reactor debounces (~500 ms) and writes atomically (tmp + rename — the root project's deferred "atomic config writes" idea, done from day one). Boot replays the store into `AddTv`-equivalent spawns. Secrets stay serde fields in vendor configs; the wire never carries them (`get-config` redacts via the vendor `config_schema` visibility tags, same as the root daemon).

Config schema: **clean break** (decided 2026-07-04) — a fresh lean schema owned by the new stack, no migration from the old daemon's files (per the project's no-backwards-compat stance); switching stacks means re-pairing once. `SystemProfileConfig`/`UserProfileConfig` semantics are ported, types reused from `tisvlay-core` where they fit without dragging shape we don't want.

### Key-space extensions (parity delta)

```
tisvlay/<site>/tv/<uuid>/state/input          {current, available[]}         cached pub
tisvlay/<site>/tv/<uuid>/state/backlight      {level}                        cached pub
tisvlay/<site>/tv/<uuid>/meta/identity        + brand, hardware{make,model,serial}, capabilities[], connector
tisvlay/<site>/tv/<uuid>/meta/config          + system_profile, primary_input, automate_when, automated
tisvlay/<site>/tv/<uuid>/rpc/…                + switch-input, set-backlight, set-screen, set-muted (exists),
                                                set-name, set-managed, set-system-profile, set-primary-input,
                                                get-config, update-config, re-pair
tisvlay/<site>/tv/<uuid>/progress/power       power-cycle phases (wol_sent → connecting → connected/failed)
tisvlay/<site>/profile/system/<uuid>          cached pub, PUT/DELETE lifecycle
tisvlay/<site>/profile/rpc/…                  create, update, duplicate, delete, defaults, effective
tisvlay/<site>/manager/vendors                queryable (vendor list + capabilities)
tisvlay/<site>/manager/vendors/<id>/schema    queryable (pairing/config schema for GUI forms)
tisvlay/<site>/manager/rpc/…                  + discover-start, discover-stop (open-ended wizard sessions)
tisvlay/<site>/discovery/<session>/found/<n>  session-scoped pubs + alive/discovery/<session> token
tisvlay/<site>/event/session/…                lock, unlock, sleep, resume (published by session service)
--- user socket only (session-service peer) ---
tisvlay/<site>/user/profile/<uuid>            cached pub + rpc CRUD
tisvlay/<site>/user/tv/<uuid>/profile         per-TV user-profile assignment
tisvlay/<site>/user/alive/session             liveliness token
```

Screen state needs no new key (`state/power.is_screen_on` already carries it); `set-screen` is the rpc.

### Reliable dispatch as systems

The `tv_manager` verify-retry-escalate machinery becomes components + Tick-driven systems: a `DispatchInFlight { action, attempts, verify_at }` component per TV; a verify system compares the target against the `Power` payload after the backend's `RetryConfig.verify_delay`; escalation (screen_on failing from a wake-preflight substate → the wake/WoL path) re-uses `PendingConnect { wake }`. Per-backend `RetryConfig` arrives with the `Connected` event alongside `supports_wake`. CEC parity rides the same rails: `reachable_in_standby` connections keep their task alive through power-off (Connected ≠ power — the world already models this naturally, since the handle only drops when the task exits), standby power reads synthesize from the cached `Power` payload, and `emulate_screen_via_power` maps screen actions to power actions at profile dispatch.

### Watchers

- **netlink** (carrier/route): port from `tisvlayd/src/netlink.rs` (transport-free) as a reactor-side adapter task → `InEvent::System`-adjacent carrier events; the world scopes reconnect reactions by `Transport::is_network_dependent` + `survives_source_ip_change` (stored per TV from connect time), reusing `PendingConnect` so carrier events cannot race WoL loops (same guarantee, no flag).
- **logind** (consumed D-Bus, stays zbus): PrepareForSleep → `Sleep`/`Resume` system events + the shutdown inhibitor. Lives in the daemon binary next to the DPMS watcher.
- **desktop_probe** (EDID ↔ connector matching): ports as-is; per-TV DPMS scoping replaces the current fleet-wide dispatch (a `MirrorsConnector(name)` component per TV; the DPMS watcher tags its events with the connector).

### Service ports

- **Session service** (`tvnext-session`): the gateway peer above + user-profile CRUD/resolution + idle detection (core's wayland `ext-idle-notify` transport, feature-gated, zbus-free) + lock/unlock monitoring (consumed D-Bus) → publishes session events; dispatches user-profile actions through the system daemon's rpcs.
- **Greeter** (`tvnext-greeter`): core wayland idle → publish `greeter/idle`. Tiny.
- **Tray** (`tvnext-tray`): ksni (zbus, unavoidable for StatusNotifierItem) + one zenoh client session; menu renders from an `AdvancedSubscriber` cache mirror; commands via rpc.
- **GUI** (`tvnext-gui`): fork of the Slint GUI into the new workspace (decided 2026-07-04): `dbus.rs`/`state_watch.rs`/wizard drain replaced by a zenoh client module (subscriber → Slint models; rpcs; wizard over pairing progress keys + discovery session pubs; profiles editor over profile rpcs; single-instance via a liveliness token on the user socket). The `.slint` UI layer ports unchanged.
- **CLI**: fill to parity (input/screen/backlight/config get+patch/re-pair/profiles/user-profiles).

### Packaging for battle-testing

Minimal first: a `tvnextd` systemd system unit (socket dir + group setup via tmpfiles), user units for session/tray, a greeter drop-in — enough to daily-drive. The NixOS module flag and nfpm packaging land at switchover, not before.

## Phases

- [x] **P1 — persistence + full TV surface**: store layer with atomic writes; boot from store; every remaining `Tv1.*` member as rpc/key (input, backlight, screen, setters, get/update-config with schema redaction, re-pair); power-cycle progress key. Exit: daemon restart keeps its TVs (restart wire test; C3 re-pair pending user return); every busctl example in CLAUDE.md has a `tvnext` equivalent.
- [x] **P2 — reliable dispatch + watchers + CEC semantics** (provisional tick): verify-retry-escalate systems (Bug-A retries + wake escalation, tested against the mock's noop knobs), netlink carrier monitor (park/redial via CarrierParked, PendingConnect as the single dial gate), logind consume + delay inhibitor (FakeLogind e2e on a private bus), desktop probe with deterministic EDID matching and per-TV connector-scoped DPMS; CEC standby model + screen-via-power verified against both the mock's CEC shape and the real `CecConnection` over the upstream `test-link` fake (feature added to `tisvlay-cec`). Field half deferred per policy: the G4 + C3 fleet session incl. suspend/resume batches to the user's return (credentials watch `/tmp/tv-creds.toml`).
- [x] **P3 — profiles + discovery sessions + schemas**: system-profile entities with PUT/DELETE lifecycle on `profile/system/<uuid>` (create/update/duplicate/delete rpcs; delete refused with `profile-in-use` while any TV references it), per-TV resolution in dispatch (assigned profile's explicit `none` suppresses the stock action, unset fields fall back per field), and the stock mapping exposed via `profile/rpc/defaults` + per-TV merge via `profile/rpc/effective`; open-ended discovery sessions (`discover-start`/`discover-stop`, per-session liveliness token, ARP-enriched findings republished each scan round on `discovery/<session>/found/*`); vendor catalog + pairing-schema queryables served bridge-side. Store round-trips profiles and assignments. Exit met: six-test wire suite (`tvnext-daemon/tests/profiles.rs`), 53 tests total. Deferred within scope: the `automated` computed flag on `meta/config` needs session presence, so it lands with P4.
- [x] **P4 — session service + greeter** (mock-verified; C3 field half batches to the user's return): `tvnext-session` gateway live — client session to the system socket (fleet mirror over `tv/*/meta/config`, session-event taps, TV rpc dispatch) + peer session owning `user/**` on the private socket (profile CRUD/duplicate/delete-guard rpcs, `defaults`/`effective` projections, per-TV assignments, liveliness token, debounced TOML store); lock/unlock via the ScreenSaver adapter (FakeScreenSaver e2e), user idle via the core wayland transport re-registering off the resolved-minimum watch. `tvnext-greeter` ported: cached `greeter/idle` publisher (presence assert + dedup) sized by the daemon's recomputed `greeter/timeout` key; daemon maps GreeterIdle/GreeterResume through system-profile `on_idle_timeout`/`on_idle_resume`. The `automated` flag landed with it: `meta/config` now carries the evaluated `automate_when` gate and dispatch skips gated TVs (both directions wire-tested). Stock mapping corrected to root parity (sleep→power_off, wake→power_on; vestigial daemon Lock/Unlock dropped — the session service owns those). Isolation exit met without a second login: two session services on one daemon, `user/**` invisible on the system socket and across user sockets (7 new wire tests; 65 total). Topology correction recorded above: user clients hold two sessions.
- [x] **P5 — tray** (field half batches to the user's return): `tvnext-tray` ports the root menu structure over a live mirror — history subscribers on `tv/**` + `profile/system/*` (system socket) and `user/**` (session socket, optional at startup), daemon liveliness driving the offline presentation, per-TV submenus keyed on powered-on (CEC standby offers wake), capability-gated sections, input radio, user+system profile pickers, managed toggle, scroll-wheel volume/backlight with middle-click target cycling and burst coalescing in the command processor, optimistic scroll writes based on the mirror. Two wire tests exercise the mirror against a real daemon + session service (hydration, live edits, assignment flow, removal tombstones, pre-connect capability gating); the ksni layer itself needs a desktop, same stance as the root harness's placeholder. Deferred to later phases: Settings/OpenGui menu item (needs the P6 GUI), desktop notifications (pairing/power-cycle toasts + category config — root `notifier.rs`/`TrayService`), tray config file, i18n (workspace-wide at promotion). 67 tests total.
- [x] **P6 — GUI** (compiles + tests green; visual QA by the user): `tvnext-gui` is the fork running on `tvnext-client`, which reproduces the root client surface signature-for-signature (streams as root item tuples, `drain_pair_to_terminal` verbatim, Body-typed resolved defaults, name-based creates, JSON-string config get/patch) — so `state.rs`/`profiles.rs`/`wizard.rs`/`power_cycle.rs` ported without logic changes and the `.slint` layer is untouched. GUI deltas: `--socket`/`--user-socket`/`--site` args behind a process-wide endpoints lookup shared by startup connect and the session-reconnect arm. **Decision superseded**: zbus stays in the GUI — `org.freedesktop.Application` single-instance/launcher activation and the `FileManager1` reveal are consumed desktop D-Bus (ksni category); the planned liveliness-token single-instance had no raise-window path. Known QA seams: a session service started *after* the GUI is only picked up via the fallback poll (start order daemon → session → gui); the Settings pane's tray-notification section reports permanently unavailable (spike tray ships no settings service); About/diagnostic show spike version strings and an empty config path. 37 suites green incl. the GUI's 84 unit tests + both headless UI suites.
- [x] **P6.5 — CLI completion**: mute, primary-input, automate-when, assign (system profile), vendors + schema reads, `profile` and `user-profile` subcommand groups (list/create/update/duplicate/delete/defaults/effective, user-side assign over `--user-socket`), and `discover-session` (open-ended scan streaming deduplicated findings until ctrl-c, then discover-stop). Smoke-tested end to end against a live daemon + session service, including the profile delete guard and effective-resolution output. Ready for user QA.
- [ ] **P7 — packaging + switchover** (user-assisted): systemd units; a week of full-stack duty; workspace promotion out of `spikes/` plus the crate-naming settlement (deferred here by decision); then the old-stack decommission decision (separate discussion — root-repo surgery is its own plan).
- [ ] **Follow-up — IPC performance harness**: side-by-side numbers for the zenoh stack vs the root D-Bus stack before switchover — settings-rpc round-trip latency under scroll bursts (tray coalescer pacing), state-fanout latency (daemon publish → client mirror), and cold-start hydration time. Sparked by C3 QA (2026-07-04): the enqueue-ack bug (fixed in f5fc7cb) *felt* like transport lag until traced; eyeballs can't distinguish transport QoS from daemon flaws, a harness can.

Battle-test checkpoints are the phase exits; the plan's war stories accumulate per phase like the spike's.

## Resolved decisions (2026-07-04)

1. **Topology: gateway design** — session service runs two zenoh sessions; user-private keys never transit the system peer.
2. **Workspace: stay in `spikes/zenoh-ecs` until switchover** — zero churn during parity; promotion and the crate-naming settlement happen together at P7.
3. **Config/state: clean break** — fresh schema, own state dir, re-pair once at switchover.
4. **GUI: fork into the new workspace** — old GUI untouched until switchover retires it.

## Risks and watch items

Carried from the spike: zenoh-ext `unstable` surface, subscriber-history-only hydration, authz-is-socket-perms (now load-bearing for greeter + multi-user), suspend/resume behavior unvalidated. New here: gateway session service is a novel pattern (validate the two-session model early in P4 with a throwaway probe if anything smells); store snapshots must never race pairing completion (persist effect ordering through the same outbox discipline); the GUI fork temporarily duplicates `.slint` sources until switchover retires the old one.
