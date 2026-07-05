# GUI state consistency: retry the no-reply law once, single-source availability

Status: **draft — under review**.

## Problem

After a stack restart, the GUI shows contradictory daemon state: the sidebar footer says online, the About pane's component row says offline, the TVs pane renders the fleet normally, and the pairing wizard opens into "Discovery failed" with an empty vendor list. Each symptom recurs independently across QA rounds; each fix so far (footer liveliness keying in c84967e, the SDK's synthesized `DaemonAppeared`) has patched one surface without closing the class.

## Root cause

One race, multiplied by N independent derivations of the same fact.

**The race.** The GUI's system link is a connect-only resilient peer: `zenoh::open` returns before the link exists, and queryable routes propagate after that. A `get` matching zero queryables completes instantly with no reply (the known zenoh law). The GUI fires its startup burst — profile baselines, vendor catalog + per-vendor schemas, About `info` probes, per-TV `get-config` — immediately after open, so every one-shot rpc in that burst can fail even when the daemon has been up for minutes (observed: daemon up 14 s, all startup rpcs failed). `DaemonClient::call_rpc` has no retry for the no-reply case; the law is encoded in the daemon's *test* helpers but not in the SDK that every client rides.

**The multiplication.** Surfaces each derive "is the daemon there" their own way, so the race freezes different answers per surface:

| Surface | Truth source | Post-race state |
|---|---|---|
| Sidebar footer | liveliness mirror (`is_available`) | online — correct |
| About component row | one-shot `info` rpc outcome, refreshed only on `DaemonAppeared`/`Disappeared` | offline forever — the synthesized `DaemonAppeared` fired inside the same racy window, spending the retry trigger |
| TVs pane | mirror subscription with history | correct — subscriptions self-heal when routes propagate; one-shot gets don't |
| Pairing wizard | startup vendors/schema fetches | "Discovery failed", empty vendor ComboBox |

## Design

Three moves at existing seams. Explicit non-goal: no GUI ECS rewrite — Slint's property system already is the reactive layer; the disease is fetch orchestration, and a new paradigm would relocate the race, not remove it.

### 1. Encode the no-reply law once, in the SDK

`call_rpc` (crates/tisvlay/src/lib.rs) retries the no-reply case: on `recv_async` returning empty, back off briefly and re-issue the `get`, up to a bounded deadline (proposed: 4 attempts over ~2 s total; first retry fast at ~100 ms since route propagation typically completes in tens of milliseconds). Genuine daemon-down still fails at the same deadline order as today. Typed error replies (`WireError`) are terminal — never retried. This heals every current and future one-shot caller (GUI, CLI via SDK-shaped helpers, tray) in one place.

Open question for review: should retry also gate on `is_available()` (skip the wait when no liveliness token has ever been seen) to keep cold-start-with-daemon-down fast, or is the bounded deadline alone simple enough?

### 2. Availability is liveliness, only

Presence comes from the liveliness mirror (`DaemonClient::is_available` / session / tray equivalents), nowhere else. Rpc outcomes render as *content* (or content-level errors), never as online/offline. Concretely:

- The About component rows key their status circle on the same `is_available` flags the footer uses; `version` / `config path` become retried content that fills in when it lands ("—" until then). The footer and About can then never disagree.
- `refresh_about_components` keeps its event-driven re-runs, but a failed probe no longer writes "offline".

### 3. Fetch discipline at the watcher seam (incremental)

The StateWatcher/mirror is the GUI's single writer for fleet state; the remaining one-shot startup fetches (baselines, vendors) already re-run on `DaemonAppeared`. With retry (move 1) those triggers become reliable, so no new machinery — but adopt the rule going forward: Slint state is written from watcher events and SDK calls (which now encode the law), never from bespoke raw `session.get` paths. Audit the GUI for direct `session.get` bypasses as part of the change.

## Implementation sketch

1. `crates/tisvlay/src/lib.rs`: retry loop in `call_rpc` (+ the same for `call_many`-shaped discovery? — review question: discovery streaming uses its own path with `ConsolidationMode::None`; a no-reply there is a legitimate empty scan result once routes exist, so proposed: retry only when `replies` closes with zero replies AND the target's liveliness token is present).
2. `crates/tisvlay-gui/src/about.rs`: component-row status from `is_available` flags; probes fill content only.
3. Audit: `grep -n 'session.get\|\.get(' crates/tisvlay-gui/src` for SDK bypasses.
4. Tests: SDK test — client connects, rpcs *before* the daemon declares queryables, call succeeds once declared (exists in daemon tests as a helper pattern; promote to an SDK-level integration test). GUI: about-components builder unit test — probe failure with `is_available == true` renders online row with "—" content.

## Risks / watch items

- Retry must not double-apply non-idempotent rpcs: a no-reply means the queryable never saw the request (no routes), so re-issuing is safe by construction — but confirm no rpc path treats "no reply" as "maybe delivered" (zenoh get with zero matching queryables does not deliver).
- The wizard's schema prefetch failure currently lands in the discovery-error banner; after move 1 it should effectively vanish, but verify the error slot still reports *real* schema failures (unknown vendor).
- `RPC_TIMEOUT` interaction: total worst-case latency = retry deadline + final attempt's timeout; keep the sum under the GUI's perceived-hang threshold (~3 s).
