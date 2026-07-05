# GUI state consistency: retry the no-reply law once, single-source availability

Status: **reviewed + probe-validated — ready to implement** (baseline 86dbc2d; amended per the design / correctness / maintainability review round, then re-based on the querier-matching primitive after an empirical probe).

## Problem

After a stack restart, the GUI shows contradictory daemon state: the sidebar footer says online, the About pane's component row says offline, the TVs pane renders the fleet normally, and the pairing wizard opens into "Discovery failed" with an empty vendor list. Each symptom recurs independently across QA rounds; each fix so far (footer liveliness keying in c84967e, the SDK's synthesized `DaemonAppeared`) has patched one surface without closing the class.

## Root cause

One race, multiplied by N independent derivations of the same fact.

**The race.** The GUI's system link is a connect-only resilient peer: `zenoh::open` returns before the link exists, and queryable routes propagate after that. A `get` matching zero queryables completes instantly with no reply (the known zenoh law). The GUI fires its startup burst — profile baselines, vendor catalog + per-vendor schemas, About `info` probes, per-TV `get-config` — immediately after open, so every one-shot rpc in that burst can fail even when the daemon has been up for minutes (observed: daemon up 14 s, all startup rpcs failed). `DaemonClient::call_rpc` has no retry for the no-reply case; the law is encoded in the daemon's *test* helpers but not in the SDK that every client rides.

Verified in review (against the zenoh 1.9 source): daemon-side declaration order (queryables, then liveliness) does **not** order client-side visibility — the token sample and the queryable route are independent propagation flows, so a client can see the token while routes are still unusable. This is why the synthesized `DaemonAppeared` re-fetch triggers spent themselves inside the racy window: re-firing without retry cannot close the race.

**The multiplication.** Surfaces each derive "is the daemon there" their own way, so the race freezes different answers per surface:

| Surface | Truth source | Post-race state |
|---|---|---|
| Sidebar footer (initial seed) | liveliness mirror (`is_available`) | online — correct |
| Sidebar footer (`refresh` path) | **`fetch_state` outcome** (state_watch.rs `refresh` writes `set_daemon_available` from Ok/Err) | can flip offline on a transient fetch failure — same anti-pattern, hot path |
| About component row | one-shot `info` rpc outcome, refreshed only on `DaemonAppeared`/`Disappeared` | offline forever |
| TVs pane | mirror subscription with history | correct — subscriptions self-heal when routes propagate; one-shot gets don't |
| Pairing wizard | startup vendors/schema fetches (`call_rpc` reads) | "Discovery failed", empty vendor ComboBox |
| `Profiles.session-available`, `Tray.available` globals | liveliness edges + initial `is_available` | already correct — the reference pattern, but written from ~7 scattered sites |

## Design

Three moves at existing seams. Explicit non-goal: no GUI ECS rewrite — Slint's property system already is the reactive layer; the disease is fetch orchestration, and a new paradigm would relocate the race, not remove it.

### 1. Querier-based rpc in `tisvlay-wire` — matching-gated calls instead of blind gets (wire v2)

The original move here was a bounded retry loop compensating for the unobservable token-seen-but-routes-not-yet window. Investigating zenoh's own primitives dissolved the problem instead: **`Session::declare_querier` + `Querier::matching_listener()` report exactly when queryables matching the querier become routable from this session** — the client-side fact retries were approximating. Probe-validated on both production topologies (the pin lives in `crates/tisvlay-wire/tests/querier_matching.rs`): matching starts false with no hub, flips true when the daemon's queryable lands (and the *immediately following* get is answered — matching ⇒ routable), flips false when the server goes; the router hop sees a client-link server's queryable the same way (the tray-settings shape). Stable across repeated runs, ~10 ms peer-hub / ~500 ms router; compiles with our existing feature set.

The rpc path becomes a shared module in `tisvlay-wire` (the vocabulary crate both the SDK and CLI already ride), replacing both hand-rolled copies — no backwards-compat constraints, all peers ship together:

- **Call semantics**: a call awaits the querier's matching status (bounded, ~2 s) before firing; not-matching at deadline fails fast with a distinct `rpc.unavailable` error. No retry loop, no liveliness latch — matching *is* the readiness signal, per verb prefix.
- **Terminal errors unchanged**: typed `WireError` replies stay terminal; zenoh's synthetic query-timeout reply (an `Ok(Err(ReplyError "Timeout"))` that fails `WireError` decode) maps to `rpc.timeout` instead of today's `unknown`.
- **Residual empty-close**: a reply channel that closes empty *after* matching was confirmed (queryable dropped mid-call, reply lost mid-flight) gets one re-issue for the idempotent read family only (`info`, `vendors`, `pairing-schema`, `get-config`, profile defaults/effective/baselines, tray `notification_settings`). Writes (`pair`, `add-tv`, `discover-start`) stay single-shot — they mint server-side state (random UUIDs, scan sessions) before the deferred ack, so re-issuing can duplicate; client-side UUID generation (already honored daemon-side via `AddTv.id`) is the follow-up that would lift this.
- **Querier granularity**: one querier per rpc *prefix* per client (`tv/*/rpc/*`, `manager/rpc/*`, `profile/rpc/*`, user-socket equivalents) — matching on the prefix covers the verb set without a querier per verb; validate during implementation that wildcard queriers report matching correctly (extend the pin test if adopted).
- **Policy home**: timeout / matching deadline constants live beside the module in `tisvlay-wire`. The CLI ports onto the shared module in this change (its `call`/`call_many` delete; `call_many`'s multi-reply consolidation-off shape moves into the module for discovery streaming), converging error rendering on `RpcError` fields so the human `code: message` format is preserved.

Discovery needs no special rule beyond the above: the wizard's scan rides a self-healing subscriber, the CLI's batch `discover` treats zero replies as a legitimate empty scan (multi-reply calls never auto-re-issue), and `discover-start` is a single-shot write. The observed "Discovery failed" symptom was never the stream — it was the vendors/schema *reads*, which matching-gating fixes.

### 2. Availability is liveliness, only — with exactly one writer

Presence comes from the liveliness mirrors (`is_available` on the daemon/session/tray clients), nowhere else. Rpc outcomes render as *content* (or content-level errors), never as online/offline. Concretely:

- One `publish_availability(app, …)` function reads the three `is_available()` flags and writes **every** Slint copy (`daemon_available`, `session_available`, `tray_available`, `Tray.available`, `Profiles.session-available`) in one place. The initial seed and all `*Appeared`/`*Disappeared` arms call it; the ~7 scattered hand-written subsets go away. This is what makes "footer and About can never disagree" actually true.
- `refresh` (state_watch.rs) **stops writing `set_daemon_available` from the `fetch_state` outcome** — the same bug as the About row, in the hot path.
- The About component rows key their status circle on the same `is_available` flags; `version` / `config path` become retried content that fills in when it lands ("—" until then). A failed probe never writes "offline". The stale "D-Bus reachable" wording in about.rs's comments gets refreshed in the same change.

### 3. Fetch discipline: state the invariant that already holds structurally

The review confirmed the GUI holds **no** raw `zenoh::Session` and imports nothing from `zenoh` — every rpc already funnels through the SDK clients. The rule to preserve is therefore structural, not a grep: **`tisvlay-gui` never holds a zenoh session or depends on `zenoh` directly** (verify Cargo.toml; drop the dep if listed unused). The useful ongoing audit is `grep -n 'set_.*_available' crates/tisvlay-gui/src` — every hit must be inside `publish_availability` — which checks the actual invariant ("only liveliness writes presence") rather than chasing `.get(` false positives.

## Implementation sketch

1. `tisvlay-wire`: new `rpc` module — querier-per-prefix management, matching-gated call (bounded await, `rpc.unavailable` on deadline), multi-reply call (consolidation off, no auto-re-issue), typed `WireError`/`RpcError` mapping incl. `rpc.timeout`; policy constants beside it. Extend `tests/querier_matching.rs` with a wildcard-prefix matching case if prefix queriers are adopted.
2. `crates/tisvlay/src/lib.rs`: `call_rpc` rides the wire rpc module (clients hold their queriers); read-family single re-issue on post-matching empty close.
3. `crates/tisvlay-cli`: delete `call`/`call_many`, port onto the wire rpc module, render errors from `RpcError` fields (keep the `code: message` shape).
4. `crates/tisvlay-gui/src/state_watch.rs`: `publish_availability` single writer; delete `refresh`'s fetch-outcome availability writes.
5. `crates/tisvlay-gui/src/about.rs`: rows keyed on `is_available`; probes fill content only; comment refresh (stale "D-Bus reachable" wording).
6. Tests — `crates/tisvlay/tests/client.rs` scaffolding (no new fixtures): (a) rpc fired *immediately after `connect`, before any liveliness wait* (inverting the existing test's wait-first ordering) asserts success within the matching deadline — never assert the first attempt would have failed, or it flakes when routes propagate fast; (b) a typed `WireError` reply (unknown vendor schema) still fails fast. GUI: about-components builder unit test — probe failure with `is_available == true` renders an online row with "—" content.
7. Docs: rewrite the architecture.md no-reply law around the querier-matching law ("clients gate rpcs on querier matching status — the pin is `tisvlay-wire/tests/querier_matching.rs`"), cross-referenced from the liveliness-after-queryables law (which stays as serve-side hygiene but is no longer load-bearing for rpc readiness); promote "availability is liveliness only; rpc outcomes are content" into architecture.md.

## Follow-ups (filed, not this change)

- Client-side UUID generation for `pair`/`add-tv` (+ a session id for `discover-start`) to make the write rpcs idempotent, unlocking re-issue for them.

## Risks / watch items

- Matching-API stability: parts of the matching surface ride zenoh's `unstable` feature (already enabled transitively via zenoh-ext) — same accepted risk class, but note it in the architecture doc's unstable-surface caveat.
- Prefix-wildcard queriers: matching semantics for a wildcard key expression against concrete queryables must be pinned by test before the per-prefix design is locked; fall back to per-verb queriers if wildcard matching over-reports.
- The wizard's discovery-error banner must still report *real* schema failures (unknown vendor) once the race noise is gone.
- CLI port changes error-output plumbing; smoke the human formats (`list`, `config`, error cases) against a live daemon before landing.
