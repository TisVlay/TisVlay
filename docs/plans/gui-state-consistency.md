# GUI state consistency: retry the no-reply law once, single-source availability

Status: **reviewed — ready to implement** (baseline 86dbc2d; amended per the design / correctness / maintainability review round).

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

### 1. Encode the no-reply law once, in the SDK — for idempotent reads

`call_rpc` (crates/tisvlay/src/lib.rs) gains a bounded retry. **Retry is the correctness mechanism** — awaiting liveliness cannot replace it, because token arrival races route propagation; any liveliness gating is a fast-fail optimization only.

Semantics pinned by the correctness review against the zenoh source:

- **Retry key**: only the empty-channel close (`recv_async` returns `Err`) — the zero-matching-queryables case. A *slow* daemon does not land here: zenoh's query timeout injects a synthetic `ReplyError("Timeout")`, which arrives as `Ok(Err(..))`, fails `WireError` decode, and stays terminal (map it to a recognizable code, e.g. `rpc.timeout`, instead of today's `unknown`). Typed `WireError` replies are always terminal.
- **Scope: idempotent reads only.** "No reply ⇒ never delivered" is *not* safe by construction: a reply lost mid-flight (link break after delivery, before the deferred ack) also closes the channel empty, and `pair` / `add-tv` / `discover-start` mint server-side state (random UUIDs, scan sessions) before replying — blanket retry could duplicate TVs or orphan discovery sessions. The retry therefore applies to the read family (`info`, `vendors`, `pairing-schema`, `get-config`, profile defaults/effective/baselines, tray `notification_settings`); the write rpcs keep single-shot semantics. The durable fix for writes — client-side UUID generation so retries upsert (`AddTv.id` is already honored daemon-side) — is filed as a follow-up, not this change.
- **Fast-fail latch**: skip the retry wait only when no liveliness token has *ever* been seen this session (cold start, daemon genuinely absent). Gate on the ever-seen latch, not the live `is_available()` flag — a token that arrived microseconds ago is exactly the window that must retry.
- **Budget**: attempts at ~100/300/600 ms backoff; the final attempt's per-call timeout is clamped to `min(RPC_TIMEOUT, 3 s − elapsed)` so even a matched-but-wedged queryable can't push a caller past the perceived-hang budget. The no-reply case returns instantly, so the typical failure cost is just the backoff window; the GUI's startup burst runs concurrently, so the budget is per rpc family, not a sum.
- **Policy home**: the constants (`RPC_TIMEOUT`, attempts, backoff, ceiling) move to `tisvlay-wire` — the vocabulary crate both the SDK and the CLI already depend on. The CLI's hand-rolled `call`/`call_many` reference the same constants but stay un-retried for now; folding the CLI onto the SDK's rpc path (and converging its error mapping on `RpcError`) is filed as a follow-up so the second copy is tracked debt, not silent drift.

Discovery needs no special retry rule: the wizard's scan rides a self-healing subscriber (`discovery_found_stream`), the CLI's batch `discover` treats zero replies as a legitimate empty scan, and the `discover-start` kickoff is a write (excluded above; its failure surfaces as a content error). The observed "Discovery failed" symptom was never the stream — it was the vendors/schema *reads*, which this move fixes.

### 2. Availability is liveliness, only — with exactly one writer

Presence comes from the liveliness mirrors (`is_available` on the daemon/session/tray clients), nowhere else. Rpc outcomes render as *content* (or content-level errors), never as online/offline. Concretely:

- One `publish_availability(app, …)` function reads the three `is_available()` flags and writes **every** Slint copy (`daemon_available`, `session_available`, `tray_available`, `Tray.available`, `Profiles.session-available`) in one place. The initial seed and all `*Appeared`/`*Disappeared` arms call it; the ~7 scattered hand-written subsets go away. This is what makes "footer and About can never disagree" actually true.
- `refresh` (state_watch.rs) **stops writing `set_daemon_available` from the `fetch_state` outcome** — the same bug as the About row, in the hot path.
- The About component rows key their status circle on the same `is_available` flags; `version` / `config path` become retried content that fills in when it lands ("—" until then). A failed probe never writes "offline". The stale "D-Bus reachable" wording in about.rs's comments gets refreshed in the same change.

### 3. Fetch discipline: state the invariant that already holds structurally

The review confirmed the GUI holds **no** raw `zenoh::Session` and imports nothing from `zenoh` — every rpc already funnels through the SDK clients. The rule to preserve is therefore structural, not a grep: **`tisvlay-gui` never holds a zenoh session or depends on `zenoh` directly** (verify Cargo.toml; drop the dep if listed unused). The useful ongoing audit is `grep -n 'set_.*_available' crates/tisvlay-gui/src` — every hit must be inside `publish_availability` — which checks the actual invariant ("only liveliness writes presence") rather than chasing `.get(` false positives.

## Implementation sketch

1. `tisvlay-wire`: rpc policy constants (timeout, retry schedule, ceiling).
2. `crates/tisvlay/src/lib.rs`: retry loop in `call_rpc` keyed on the empty-channel close, read-family scope, ever-seen latch, final-attempt clamp; map zenoh's synthetic timeout reply to `rpc.timeout`.
3. `crates/tisvlay-gui/src/state_watch.rs`: `publish_availability` single writer; delete `refresh`'s fetch-outcome availability writes.
4. `crates/tisvlay-gui/src/about.rs`: rows keyed on `is_available`; probes fill content only; comment refresh.
5. Tests — in the existing `crates/tisvlay/tests/client.rs` scaffolding (no new fixtures): (a) rpc fired *immediately after `connect`, before any liveliness wait* (inverting the existing test's wait-first ordering) asserts eventual success within the retry deadline — never assert the first attempt fails, or it flakes when routes propagate fast; (b) a typed `WireError` reply (unknown vendor schema) still fails fast, proving retry doesn't swallow terminal errors. GUI: about-components builder unit test — probe failure with `is_available == true` renders an online row with "—" content.
6. Docs: amend the architecture.md no-reply law ("the SDK retries this — callers get it free; the CLI carries the same policy constants") and cross-reference it from the liveliness-after-queryables law (server-side half vs client-side mitigation); promote "availability is liveliness only; rpc outcomes are content" into architecture.md.

## Follow-ups (filed, not this change)

- CLI → SDK rpc-path consolidation (delete the CLI's `call`/`call_many`, converge on `RpcError`); until then the CLI shares constants but not the retry loop.
- Client-side UUID generation for `pair`/`add-tv` (+ a session id for `discover-start`) to make the write rpcs idempotent, unlocking retry for them.

## Risks / watch items

- The retry test is timing-shaped: assert eventual success within the deadline only.
- The wizard's discovery-error banner must still report *real* schema failures (unknown vendor) once the race noise is gone.
- Worst-case wall clock: ever-seen latch keeps cold-start-down fast; the final-attempt clamp keeps daemon-wedged under ~3 s; verify both in the SDK test if cheap.
