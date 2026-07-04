# Parity: driving the zenoh+ECS stack to full TisVlay parity

Status: **baseline committed — open questions pending user answers** (see Open decisions).

## Goal

The spike ([spike-zenoh-ecs.md](spike-zenoh-ecs.md)) exited with a keep verdict: continue the zenoh + bevy_ecs stack to complete feature parity with the zbus stack, so it can be battle-tested end to end — daemon, CLI, session service, greeter, tray, and GUI — as a daily driver. The old stack stays runnable until the new one has survived real duty (switchover criteria at the bottom).

## Architecture decisions

### Two-socket topology and the transitive-routing pitfall

The zbus stack separates a system bus (daemon, policy-gated) from per-user session buses (session service, user profiles). The zenoh approximation needs care because **zenoh peer routing is transitive**: if per-user session peers and the system daemon form one connected mesh, any client anywhere in the mesh can subscribe to any key — user B could read user A's `user/**` keys straight through the system daemon. Separate sockets alone do not reproduce D-Bus's session isolation.

Proposed design — **the session service is an application-level gateway, not a router**:

- **System daemon**: zenoh peer, listener on `/run/tisvlay/zenoh.sock` (0660, group `tisvlay`). Owns `tisvlay/<site>/**` except the `user/` subtree.
- **Per-user session service**: runs **two zenoh sessions**. Session A: client → system socket (consumes TV state, publishes session events on `tisvlay/<site>/event/session/*`, calls rpcs). Session B: peer with a listener on `$XDG_RUNTIME_DIR/tisvlay/session.sock` (0700 runtime dir = per-user privacy for free), owning the `tisvlay/<site>/user/**` subtree (user profiles, per-TV assignments, its own liveliness token). Nothing routes between A and B except explicit application logic — user-private keys physically never reach the system peer.
- **User clients (GUI, tray, CLI)**: one zenoh client session with **two connect endpoints** (system socket + own user socket). Client mode does not forward between its links, so the mesh stays partitioned; one subscriber on `tisvlay/<site>/**` spans both worlds transparently.
- **Greeter**: client → system socket only (greeter user joins the `tisvlay` group), publishing `tisvlay/<site>/greeter/idle` exactly as the spike does.

Alternatives considered: (a) single connected mesh + zenoh ACL rules denying `user/**` across the system daemon's links — keeps one session everywhere but rests the privacy boundary on `unstable` ACL semantics instead of the filesystem; (b) single system socket, user keys namespaced by uid — no privacy at all between logged-in users. The gateway design is recommended; decision pending (question 1).

Carried risk, now sharper with multi-user in scope: within one socket's group, there is no per-method or per-caller authorization (the greeter user can technically publish anything on the system mesh). This was the accepted regression from the original spike plan; polkit-shaped gating would be a post-parity design if it ever matters.

### Persistence

The daemon gains a `store` layer: a TOML file (`/var/lib/tisvlay/…` in production, `--state-dir` for dev) holding TVs (including vendor configs with credentials), per-TV settings, and system profiles. The world emits a `Persist` effect (full snapshot) whenever config-shaped components change; the reactor debounces (~500 ms) and writes atomically (tmp + rename — the root project's deferred "atomic config writes" idea, done from day one). Boot replays the store into `AddTv`-equivalent spawns. Secrets stay serde fields in vendor configs; the wire never carries them (`get-config` redacts via the vendor `config_schema` visibility tags, same as the root daemon).

Config schema: **clean break** (pending question 3) — a fresh lean schema owned by the new stack, no migration from the old daemon's files (per the project's no-backwards-compat stance); switching stacks means re-pairing once. `SystemProfileConfig`/`UserProfileConfig` semantics are ported, types reused from `tisvlay-core` where they fit without dragging shape we don't want.

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
- **GUI** (`tvnext-gui`): fork of the Slint GUI into the new workspace (pending question 4): `dbus.rs`/`state_watch.rs`/wizard drain replaced by a zenoh client module (subscriber → Slint models; rpcs; wizard over pairing progress keys + discovery session pubs; profiles editor over profile rpcs; single-instance via a liveliness token on the user socket). The `.slint` UI layer ports unchanged.
- **CLI**: fill to parity (input/screen/backlight/config get+patch/re-pair/profiles/user-profiles).

### Packaging for battle-testing

Minimal first: a `tvnextd` systemd system unit (socket dir + group setup via tmpfiles), user units for session/tray, a greeter drop-in — enough to daily-drive. The NixOS module flag and nfpm packaging land at switchover, not before.

## Phases

- [ ] **P1 — persistence + full TV surface**: store layer with atomic writes; boot from store; every remaining `Tv1.*` member as rpc/key (input, backlight, screen, setters, get/update-config with schema redaction, re-pair); power-cycle progress key. Exit: daemon restart keeps the C3 paired; every busctl example in CLAUDE.md has a `tvnext` equivalent.
- [ ] **P2 — reliable dispatch + watchers + CEC semantics**: verify-retry-escalate systems, netlink adapter, logind consume + inhibitor, desktop_probe with per-TV DPMS scoping; CEC standby model + screen-via-power; upstream `test-link` feature in `tisvlay-cec` so the spike gains CEC coverage. Exit: G4 + C3 fleet field session incl. suspend/resume survives.
- [ ] **P3 — profiles + discovery sessions + schemas**: system-profile entities/CRUD/resolution with stock seeds; open-ended discovery sessions (pubs + liveliness); vendor/schema queryables. Exit: everything the GUI wizard and profiles editor need exists on the wire.
- [ ] **P4 — session service + greeter**: gateway topology live (two sockets), user profiles, idle/lock events, greeter port. Exit: lock/unlock/idle drive the C3 through user-profile resolution with two users logged in seeing no cross-talk.
- [ ] **P5 — tray**: port + field use. Exit: tray is the daily driver for power/volume.
- [ ] **P6 — GUI**: forked port per above. Exit: pairing wizard, TVs pane, profiles pane all functional against the new daemon.
- [ ] **P7 — CLI completion + packaging + switchover**: remaining CLI verbs; systemd units; a week of full-stack duty; then the old-stack decommission decision (separate discussion — root-repo surgery is its own plan).

Battle-test checkpoints are the phase exits; the plan's war stories accumulate per phase like the spike's.

## Open decisions (asked alongside this baseline)

1. Topology: gateway design vs single-mesh + ACL vs single socket (recommendation: gateway).
2. Workspace promotion + naming: stay in `spikes/zenoh-ecs` vs promote to a top-level workspace now; keep `tvnext-*` names or rename.
3. Config/state: clean-break schema (recommendation) vs sharing the root daemon's config format/state dir.
4. GUI: fork into the new workspace (recommendation) vs port `crates/tisvlay-gui` in place.

## Risks and watch items

Carried from the spike: zenoh-ext `unstable` surface, subscriber-history-only hydration, authz-is-socket-perms (now load-bearing for greeter + multi-user), suspend/resume behavior unvalidated. New here: gateway session service is a novel pattern (validate the two-session model early in P4 with a throwaway probe if anything smells); store snapshots must never race pairing completion (persist effect ordering through the same outbox discipline); the GUI fork temporarily duplicates `.slint` sources until switchover retires the old one.
