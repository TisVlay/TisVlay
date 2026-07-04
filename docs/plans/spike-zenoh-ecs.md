# Spike: Zenoh + bevy_ecs rewrite exploration

Status: **drafting** — plan under review, no spike code yet.

## Goal

Explore how much simpler and more pleasant the TisVlay stack becomes if the daemon's internal orchestration moves to an ECS world (`bevy_ecs`) and the IPC moves from served D-Bus to Zenoh. This is an architecture experiment, not a migration: the deliverable is a working vertical slice plus a keep/kill verdict, and the existing zbus stack keeps running the house untouched throughout.

Guiding principle (decided upfront): **lean fully into each technology's idioms**. No transport-abstraction layer, no lowest-common-denominator wire trait held open for zlink/tokio-DIY. If Zenoh has a native shape for a problem, we use it. Later spikes (zlink first candidate) get the same treatment in their own plan docs, and comparison happens between finished idiomatic artifacts, not through a shared abstraction.

### Why rewrite the middle at all

- The served-D-Bus middle (interface registration, cached properties, PropertiesChanged forwarding, `a{sv}` coercion, JSON↔TOML wire conversion, object-path encoding) is ~9.3k lines in explicitly D-Bus-named modules and realistically 12–15k of the ~58k-line workspace once `tv_manager`'s registration choreography and client-side watchers are counted.
- Several Known Quirks in CLAUDE.md are theorems about zbus, not about TVs: the `ObjectServer::at` → `Ok(false)` silent-refresh trap, the unregister-before-register dance, the deferred-`complete`-until-interfaces-registered ordering, the per-TV Mutex drain layer, `PairingProgressDetail` over `a{sv}`.
- The hardest-won code — `tisvlay-lg` transport/protocol, `tisvlay-cec` link/identity, WoL, netlink, DPMS, pairing semantics — has zero D-Bus in it and ports verbatim.

### Contenders and sequencing

| Candidate | Verdict for now |
|---|---|
| **Zenoh 1.9** | First spike (this plan). Richest data model (pub/sub + queries + computations + liveliness), native cross-host future. |
| **zlink (Varlink)** | Second spike if desired. Strong Linux-ecosystem backing (systemd lane, zbus author, Meta-sponsored) but actively under construction — deliberately given time to polish while the Zenoh spike runs. |
| **tokio UDS DIY** | Control group; only if both frameworks disappoint. |

## Zenoh 1.9 model primer (grounded in the local checkout at `~/ws/lgtv/libs/zenoh`)

Primitives we lean on, verified against the 1.9.0 source:

| Primitive | What it gives us |
|---|---|
| Key expressions (`*`, `**`, verbatim chunks) | The entire addressing model; clients subscribe to slices (`tisvlay/main/tv/*/state/**`). |
| `AdvancedPublisher` with `cache(CacheConfig)` | Every state key doubles as its own snapshot: the embedded cache answers gets and history queries. Replaces cached properties **and** `GetManagedObjects`. |
| `AdvancedSubscriber` with `history(...detect_late_publishers())` + `recovery(...heartbeat())` | Late-joiner hydration (StateWatcher's snapshot-then-subscribe as a protocol feature) and sample-miss recovery; `sample_miss_listener` for observability. |
| Queryables (`declare_queryable(...).complete(true)`) | RPC and computed reads ("computations"): a queryable's reply can be calculated, not stored — pairing schemas, redacted config reads, ARP enrichment. |
| Declared `Querier` + `reply()` / `reply_err()` | Client-side repeated RPC with typed error payloads; `reply.result()` is `Result<Sample, ReplyError>` — daemon error enums serialize into `reply_err`. |
| PUT / DELETE sample kinds | Entity lifecycle on the wire: first PUT of `tv/<uuid>/meta/identity` ≙ `TvAdded`; DELETE of the key subtree ≙ `TvRemoved`. No ObjectManager. |
| Liveliness tokens + liveliness subscribers | Presence: daemon-up, pairing-session-alive, discovery-session-alive. Token drop on crash = free abort detection (replaces `PairingSessionGuard`'s client-visible half). |
| Matching listeners (`matching_status`) | Publisher knows whether anyone listens — optional optimization to skip serializing state deltas with no clients attached. |
| Attachments | Side-channel metadata (trace/correlation ids) without polluting payloads. |

Deliberately **not** used: `zenohd` router, storage plugins, SHM, QUIC/TLS links. The daemon is a peer with a `unixsock-stream` listener; there is no external broker.

Config facts that matter (from `DEFAULT_CONFIG.json5`):

- Multicast scouting and gossip are **enabled by default** — both must be disabled or the daemon starts discovering peers on the LAN. Endpoints are pinned explicitly.
- `queries_default_timeout: 10000` ms — long-running operations must not be modeled as blocking queries (see RPC pattern below).
- ACL interceptors can filter on link protocol (including `unixsock-stream`) but identity means transport usernames/cert CNs/ZIDs — **there is no per-uid caller identity**. The authorization boundary is Unix socket file permissions. Accepted for the spike; see Security stance.

## Core architecture: component ↔ key ↔ payload

The load-bearing design sentence: **each unit of TV state is one serde struct that is simultaneously the bevy component, the wire payload, and the value at one key**.

- `#[derive(Component, Serialize, Deserialize)] struct VolumeState { level: u8, muted: bool }` lives at `tisvlay/<site>/tv/<uuid>/state/volume`.
- A `publish_changes` system queries `Changed<VolumeState>` and emits a put on exactly that key. This replaces the entire `state_forwarder.rs` pipeline.
- The `AdvancedPublisher` cache makes the key self-snapshotting; `z get 'tisvlay/**'` in a shell dumps the whole world state, human-readable (payload encoding: `Encoding::APPLICATION_JSON` + `serde_json` for the spike — debuggability first; a compact encoding is a later payload-crate-local swap).
- Coupling the wire crate to `bevy_ecs` (dual derive) is intentional lean-in, not an accident to fix.

### Process topology

- `tvnext-daemon` (placeholder name): zenoh **peer**, listener on a Unix socket (`$XDG_RUNTIME_DIR/tisvlay/next.sock` in dev; `/run/tisvlay/zenoh.sock`, mode 0660, group `tisvlay` in a hypothetical production). Owns the ECS world.
- Clients (`tvnext-cli`, later GUI/tray): zenoh **client** mode, `connect.endpoints` pinned to the daemon socket, scouting off.
- Consumed D-Bus stays consumed: logind, ScreenSaver, StatusNotifierItem (tray is ksni and therefore zbus forever). Those adapters feed the world as inbound events; zbus remains a dependency of those binaries only.
- The per-user session service is out of spike scope; its future shape (second listener endpoint, own key subtree `tisvlay/<site>/user/<uid>/...`) is an open question below.

### The reactor: tokio ↔ world bridge

Nothing async runs inside a system. All I/O lives in tokio tasks; the world runs at sync points:

```
loop {
    let batch = inbox.recv_many().await;        // zenoh queries/samples, backend events, logind/dpms/netlink, ticks
    apply(batch, &mut world);                    // write messages / components into the world
    schedule.run(&mut world);                    // systems: reconcile, dispatch, publish_changes, ...
    for effect in world.resource_mut::<Outbox>().drain() {
        spawn_or_send(effect);                   // zenoh puts/replies, backend commands, connect tasks
    }
}
```

- Inbound: one mpsc `WorldInbox`. Zenoh queryable/subscriber callbacks, per-TV backend event channels (the existing `TvConnection` push-subscription seam, unchanged), and the watchers (logind, DPMS, netlink) all send typed `InEvent`s.
- Outbound: an `Outbox` resource of effects. Systems never touch sockets.
- RPC queries: the zenoh `Query` handle rides the inbox into the world, systems compute, the reply is an outbox effect. A `Query` dropped unreplied finalizes empty — replies are explicit effects, so this is visible in tests.
- Timers (WoL retry, settle delays): a tokio interval feeds `Tick` events; retry state is components, cadence is a system run condition. No timers inside the world.

Watch item (the spike's central risk): this bridge must stay smaller and clearer than the `state_forwarder` + zbus glue it replaces. If channel choreography starts re-growing that complexity in a new costume, that is a kill signal.

### ECS world inventory (initial sketch)

Entities: one per TV; one per pairing session (`ChildOf` the TV — despawn cascade is the RAII guard); one per discovery session; one per system profile; one per DRM connector (relationship to the TV it mirrors).

Components (state group ↔ key, per the design sentence): `PowerState`, `VolumeState`, `InputState`, `ScreenState`, `BacklightState`, `LinkState { connected, reconnecting, supports_wake }`, `TvIdentity { name, vendor, brand, hardware_id, capabilities, connector }`, `TvConfigMeta { managed, system_profile, primary_input, automate_when }`, plus non-wire components: `ConnectionTask { cmd_tx }`, `Reconnecting` (marker), `PairingSession`, `WolRetry`.

Systems (Update schedule, ordered): `ingest_inbox` → `handle_rpcs` → `reconcile_connections` (managed ∧ ¬connected ∧ ¬reconnecting → spawn connect effect + insert `Reconnecting`) → `wol_retry` → `dispatch_profile_actions` (logind/dpms/idle events × per-TV profile → backend command effects) → `publish_changes` (`Changed<T>` → puts; `RemovedComponents`/despawns → DELETEs) → `update_liveliness`.

bevy_ecs features to exercise deliberately (lean-in), grounded against the local checkout at `~/ws/lgtv/libs/bevy` (main = **0.20.0-dev** — pin the latest release at spike start, and note the API is well past older docs): change detection (`Changed<T>`, `RemovedComponents`); marker components; relationships (`ChildOf`/`Children` — parent despawn cascades to descendants, which is the pairing-session RAII story); observers on lifecycle events (`On<Despawn>`/`On<Remove>` of TV entities emit the wire DELETE effects); **messages** (`MessageWriter`/`MessageReader` — the buffered-event API, renamed from the old `Event`/`EventReader`, and the natural shape for the inbox drain); custom **disabling components** (`world.register_disabling_component::<Unmanaged>()` — unmanaged TVs vanish from every system's queries by default while keeping their entity, state, and relationships intact); run conditions.

## Key space

`<site>` is a config-chosen chunk (default `main`). It is included **now** because retrofitting a chunk breaks every key literal, and the cross-host future (daemon on a Pi beside the G4) is a core reason Zenoh was chosen. Clients subscribe `tisvlay/*/tv/**` and are site-agnostic from day one.

```
tisvlay/<site>/manager/info                      cached pub   {version, config_path}
tisvlay/<site>/manager/vendors                   queryable    supported vendors + capabilities (computed)
tisvlay/<site>/manager/vendors/<id>/schema       queryable    pairing schema (computed)
tisvlay/<site>/manager/rpc/<verb>                queryable    pair, cancel-pair, reload-config, enrich-manual-entry, ...
tisvlay/<site>/tv/<uuid>/meta/identity           cached pub   TvIdentity (first PUT ≙ TvAdded; DELETE ≙ TvRemoved)
tisvlay/<site>/tv/<uuid>/meta/config             cached pub   TvConfigMeta (vendor secrets never on the wire)
tisvlay/<site>/tv/<uuid>/state/<field>           cached pub   power | volume | input | screen | backlight | link
tisvlay/<site>/tv/<uuid>/rpc/<verb>              queryable    power-on/off, set-volume, set-muted, set-screen, set-backlight, switch-input, set-name, set-managed, set-system-profile, set-primary-input, get-config, update-config, reconnect, re-pair, remove
tisvlay/<site>/tv/<uuid>/progress/power          plain pub    power-cycle phases (transient)
tisvlay/<site>/pairing/<uuid>/progress           cached pub   full-session history — late wizard attach replays phases; terminal sample carries the typed failure reason
tisvlay/<site>/discovery/<session>/found/<id>    cached pub   discovered TVs, session-scoped
tisvlay/<site>/profile/system/<uuid>             cached pub   profile body (PUT/DELETE lifecycle)
tisvlay/<site>/profile/rpc/<verb>                queryable    create, update, duplicate, delete, effective, defaults
tisvlay/<site>/event/<name>                      plain pub    dpms-off/on, lock/unlock — observability taps
tisvlay/<site>/greeter/idle                      plain pub    greeter → daemon (bool)
liveliness:  tisvlay/<site>/alive/daemon · alive/pairing/<uuid> · alive/discovery/<session>
```

### RPC and lifecycle patterns

- **Mutations** are gets against `rpc/<verb>` queryables with a serde payload; reply is `Ok(payload)` or `reply_err(TvError)` — the typed-failure-reason work carries over as a plain serde enum. Queryables are declared `complete(true)`.
- **Slow operations return fast.** Anything that can exceed seconds (pair, re-pair, reconnect+WoL, power cycle) validates, spawns, and replies immediately; progress streams on a `progress/` key. This respects the 10 s query-timeout ceiling and matches today's async-spawn AddTv/RePair contract.
- **Pairing**: `manager/rpc/pair` → immediate `{tv_id}` reply; progress on `pairing/<uuid>/progress` (cache holds the whole session so a re-opened wizard replays it — the WizardGen generation-counter dance should mostly dissolve); liveliness token `alive/pairing/<uuid>` for daemon-crash/abort detection; `rpc/cancel-pair` to stop. Terminal `complete` needs no interface-registration deferral: the TV's keys are cached pubs, so a client acting on `complete` hydrates whenever it likes.
- **Discovery**: open-ended wizard scans use a session (`rpc` start/stop + `discovery/<session>/found/*` pubs + liveliness token). A one-shot bounded scan may alternatively be a single get with streamed replies — decide in M2 by trying both; the query-timeout ceiling likely rules it out for wizard use.
- **Removal**: `rpc/remove` despawns at the next sync point; an observer emits DELETE samples across the TV's subtree. In-flight RPCs against a despawned uuid get `reply_err(tv-not-found)` naturally — the Mutex-drain theorem is replaced by sync-point mutation.

## Obituaries and new theorems

Dies with the rewrite: `ObjectServer::at` `Ok(false)` trap + unregister-dance + collision test; deferred-complete-until-registered; per-TV Mutex drain layer + invariant test; `a{sv}` coercion + `PairingProgressDetail` encoding + `json_toml.rs` (wire half; disk TOML stays trivially serde); underscore-encoded object paths + `dbus_paths.rs`; `state_forwarder.rs`; most of `dbus_client.rs` (StateWatcher machinery); `PrivateBus`-based wire tests (per-test UDS endpoints need no external `dbus-daemon`).

New theorems we take on (honesty ledger): cache/history sizing and staleness semantics; miss-recovery cadence (heartbeat vs periodic queries); bridge backpressure and inbox ordering; query-timeout tuning; zenoh session behavior across suspend/resume; keeping the reactor loop responsive under slow backend effects.

## Security stance

Socket file permissions (0660, group) are the whole boundary; Zenoh's ACL cannot see uids and per-method policy from `org.tisvlay.Daemon.conf` has no equivalent. Accepted consciously for the spike (single-user desktop). If the rewrite graduates, the multi-user/polkit idea needs its own design (documented gap, not a silent one). The greeter's method-level allowlist becomes: greeter user is in the socket group and can technically publish anything — flagged, accepted for now.

## What is out of scope

Cross-host operation (the `<site>` chunk is the only concession), session-service and GUI/tray ports, user profiles, i18n, packaging, polkit. The root workspace is untouched: spike code lives in `spikes/zenoh-ecs/` as an **independent cargo workspace** (placeholder crate names `tvnext-wire`, `tvnext-world`, `tvnext-daemon`, `tvnext-cli`), path-depping `tisvlay-core`/`-mock`/`-lg`/`-cec` from the parent tree so real backends work without copying. M0 verifies core's feature flags allow building without its zbus-gated modules.

## Milestones

- [ ] **M0 — scaffold**: spike workspace, deps pinned (zenoh 1.9.x, latest bevy_ecs), path-deps building with core's D-Bus features off, daemon opens a zenoh peer session on a UDS endpoint with scouting disabled, liveliness token visible from `z sub`.
- [ ] **M1 — world core (no wire)**: TV entities from config, mock-backend connect/reconnect/pairing as systems + tasks through the reactor, WoL-retry and profile-dispatch systems, headless world tests (feed `InEvent`s, run schedule, assert components + Outbox). Exit: the remove-during-in-flight scenario is a boring test, not an invariant essay.
- [ ] **M2 — wire**: state keys with AdvancedPublisher caches, rpc queryables with typed errors, PUT/DELETE lifecycle, pairing progress + liveliness, late-joiner hydration demo (`z get 'tisvlay/**'`, `z sub` with history), discovery both ways (session pubs vs streamed-reply get) with a decision recorded.
- [ ] **M3 — hardware in the loop**: LG backend via path-dep, pair the C3, power/volume/input round-trip, WoL retry against the real quirks, DPMS watcher feeding the world; CEC at least via `FakeCecLink`.
- [ ] **M4 — first real client**: CLI subset (list/status/power/volume/watch) as a pure zenoh client; side-by-side DX comparison against `dbus_client.rs` equivalents.
- [ ] **Exit review**: score keep/kill criteria below; append verdict + war stories here; then either draft the continuation plan (session/GUI ports) or the zlink spike plan.

Keep signals: world tests read like specs; bridge stayed thin; `z` CLI debugging beats `busctl` muscle memory; state hydration "just works" for late joiners.

Kill signals: bridge choreography rivals the plumbing it replaced; zenoh dep weight/compile times grate; suspend/resume or cache semantics produce mystery staleness; the authz gap starts blocking features rather than being a deferred note.

## Open questions

1. Topology when the session service arrives: clients connecting to two endpoints (system + user socket) in one zenoh session vs an all-peer mesh — defer until a session-service port is on the table.
2. Key-space versioning: none for the spike; decide before anything external consumes the keys.
3. Tick source for timers: plain tokio interval events vs `bevy_time` with a manually advanced clock — decide in M1 by feel.
4. Discovery via streamed query replies vs session pubs — M2 decision point.
5. Whether `matching_status` gating (skip publishing with zero subscribers) is worth its complexity — measure, probably skip.
6. Payload encoding evolution (JSON → CBOR/postcard) once debugging comfort is established.
