# Zenoh idiom audit (2026-07-12)

Codebase-wide audit of TisVlay's zenoh usage against zenoh 1.9.0 ("Longwang") idioms, prompted by the discovery that one recorded empirical law (the serve-side "consume-only peer" law) was stale/overbroad and had driven a heavier-than-necessary workaround (the remote-session mirror). Question: is that the only stale law, and did it (or others) drive workarounds elsewhere? Method: four read-only agents — one re-verified every recorded law against extracted 1.9.0 source, three inventoried and idiom-classified every zenoh call site (daemon; session; wire/tray/SDK/greeter/cli/gui). Report-first: nothing is changed here; this is the triage list.

## Verdict

**The codebase is overwhelmingly idiomatic.** No law besides the already-corrected serve-side one is stale — all eight mechanical laws re-verified true against 1.9.0 source. The corrected law has exactly **one remaining artifact**: the `remote/session` mirror, which `f37a954` partially addressed (the crash-clear token) but which a cached publisher would fully resolve, retiring a subscriber, a re-put path, and the boot-startup gate. Two of the three patterns most at risk of being stale-law artifacts (`client_config_resilient`, the tray dual-link, the `Caller` retry) are confirmed correct and must **not** be "fixed". One genuinely new correctness smell surfaced independent of the stale law (`call_tv` bypasses the shared RPC matching gate).

## Part A — Recorded laws, re-verified against zenoh 1.9.0 source

All confirmed unless noted. Source cites are `<crate>-1.9.0/src/...` (extracted from the cached `.crate` tarballs; the registry had no unpacked copy).

| Law | Verdict | Note |
|---|---|---|
| Client keeps a single link (`single_link:true`); multi-endpoint = failover → two sessions per client | STILL-TRUE | `net/runtime/orchestrator.rs:193,361-391` — client returns after the first link. |
| AdvancedPublisher cache is subscriber-history-only (`@adv` verbatim suffix; plain gets miss it) | STILL-TRUE | `zenoh-ext/advanced_cache.rs:149,232`; `@`-chunks are wildcard-verbatim (`keyexpr/intersect`). |
| Multi-reply streaming needs `ConsolidationMode::None` | STILL-TRUE | `zenoh-protocol/zenoh/query.rs:23-41`; default `Auto` drops same-key replies. |
| `timestamping/enabled` required daemon-side for cache ordering | STILL-TRUE — **understated** | Not just ordering: a cached AdvancedPublisher **hard-`bail!`s at construction** without an HLC (`advanced_publisher.rs:366-373`), and per-role default is router:true / **peer:false** / client:false (`zenoh-config/defaults.rs:141`). The daemon's system socket is a peer → must explicitly enable, or the publisher won't build. **Sharpen the doc.** |
| No cross-key sample ordering (→ combined-predicate single-drain tests) | STILL-TRUE | Conservative-by-design: zenoh guarantees only per-key ordering; there is no cross-key mechanism to have regressed. |
| A get matching zero queryables returns instantly, no reply (→ gate on querier matching) | STILL-TRUE | `Querier::matching_status()` present (`api/querier.rs:207`); the `Caller` gate rides it. |
| Zenoh rejects tokio current-thread scheduler | STILL-TRUE | Literal `panic!` in `zenoh-runtime/lib.rs:149`. |
| Stale Unix socket files fine (`.lock` takeover on next bind) | STILL-TRUE | `unixsock_stream/unicast.rs:304-350` — flock-guarded takeover; a live owner still refuses. |
| Serve-side (already corrected): dialed hub observes a connect-only peer's served entities; fan-out to *other* leaves still needs client→router | CORRECTION ACCURATE | Regions rework is the Longwang (1.9.0, 2026-04-16) headline; the correction text is sound. |
| Reply-side backpressure (`CommandDone` acks) | APP-LEVEL | TisVlay's Coalescer model, not a zenoh law; underpinning (`CongestionControl::Block` default) intact. |

**Doc refinements (Part A):**
- **Law 4** — rewrite "for cache ordering" to note the build-time failure, but scoped correctly (per the B1 experiment): a *bare* `.cache()` AdvancedPublisher fails to construct without an HLC (timestamping on for the node's role), BUT the project's standard shape `.cache(...).sample_miss_detection(...)` overrides sequencing to a plain counter and so **constructs regardless of timestamping**. The build failure only bites a cache-only publisher. State both halves so nobody adds a spurious timestamping requirement to a connect-only-peer publisher.
- **Law 5** — note it is a conservative no-guarantee, not a documented positive behavior.
- **Provenance** — the serve-side law's out-of-doc note calls its origin "pre-1.0 (Firesong)"; Firesong is 1.0.0 (Oct 2024), and the *observation* change came with the Regions rework in 1.9.0/Longwang. The law was accurate for zenoh 1.0–1.8 and went stale at 1.9. The in-`architecture.md` correction already reads correctly; only the note (and the memory) drift.

## Part B — Usage findings, ranked

### B1 (highest) — `remote/session` mirror: plain-put → cached AdvancedPublisher

*Depends on the (now-corrected) serve-side law.* Confirmed from both the daemon and session sides.

The session mirrors the armed **set** to the daemon as a plain `system.put` with no cache (`session/lib.rs:650`); the daemon subscribes plain/no-history (`daemon/lib.rs:379`). Because a plain put leaves nothing to hydrate, a daemon restart loses the armed set, which is why the daemon carries the `StartupReadiness` gate (`~120` lines, `daemon/lib.rs:124-189,595-615`) that stalls the boot WoL 1–3 s waiting for the session to re-publish — and the session carries the `daemon_alive` subscriber + `mirror_remote()` re-put purely to drive that re-publish (`session/lib.rs:244-249,432-447,643`). A residual correctness hole remains: a session that reconnects but never re-mirrors leaves the daemon on a stale/empty armed set until the 3 s fallback fires a possibly-wrong remote-room wake.

**Idiomatic form:** the session serves `remote/session` as a **cache-1 AdvancedPublisher** (it already owns the publisher machinery on the user side, `session/lib.rs:1046`); the daemon's subscriber gains `.history(true)`. On restart the daemon **hydrates the last armed set directly from the session's `@adv` cache** — exactly what the re-put race is trying to obtain. This retires `mirror_remote()`, the `daemon_alive` subscriber + its select arm, the clean-exit put (`session/lib.rs:458`), and most/all of the `StartupReadiness` gate. The `session_alive` token from `f37a954` stays — it does the crash-clear (a token is presence; this mirror is content — a *set* — so a cached pub is the right primitive, not a token).

**Empirical pin — RESOLVED, B1 is VIABLE (2026-07-12, 23-run experiment).** The `@adv` advanced-cache queryable served by a **connect-only peer** IS reachable by the daemon's (dialed-hub) history querier over the directly-dialed link. Hydration works first-try, every run, ~1 ms, zero flakiness — including the literal restart path (daemon closes its whole session, a fresh peer rebinds the socket via `.lock` takeover, the resilient session re-dials, and the fresh subscriber hydrates the standing armed value from the session's cache). A plain (no-history) subscriber correctly misses the cache, confirming the mechanism distinction. So B1 may proceed: the daemon can drop the `StartupReadiness` gate and the session can drop `mirror_remote()` + the `daemon_alive` re-put; the daemon hydrates the last armed set on restart within ~1 ms. The `session_alive` token stays (crash-clear presence, orthogonal).

**Config nuance (also corrects A4 below):** the real B1 needs **no `timestamping` change**. The project's builder shape `.cache(...).sample_miss_detection(...).publisher_detection()` sets `Sequencing::SequenceNumber` (an atomic counter, no HLC) *after* `.cache()` sets `Sequencing::Timestamp`, so the HLC `bail!` is dead code for this shape — verified: the ts-off connect-only-peer config both constructs and hydrates. Only a bare `cache`-only publisher hits the A4 build failure. The daemon subscriber needs `.history(HistoryConfig::default().detect_late_publishers())`; that is sufficient.

Cost: cross-crate (session + daemon + wire), medium; **deletes more than it adds** (the re-put machinery, and simplifies the gate). Also fixes the doc bug in B2. B1 refines the remote-session feature's own mirror/gate (from `f37a954`), so it and the other B items land on the feature branch with the feature PR.

### B2 — Stale doc comment on `keys::remote_session`

*Depends on the same law.* `wire/lib.rs:378-380` still asserts the old justification verbatim: "a plain put (no cache), since the session's system link is a connect-only peer and served entities do not propagate over it." This directly contradicts the corrected `keys::session_alive` comment (`wire/lib.rs:277-279`) that `f37a954` fixed, and the corrected architecture law. **Trivial fix**, independent of B1's plumbing — do it regardless (and it's the natural companion to B1).

### B3 — `call_tv` bypasses the shared `caller::Caller` matching gate

*Independent of the stale law; depends on the still-valid querier-matching law.* NON-IDIOMATIC. The session's TV RPC uses a raw `system.get(...).timeout(5s)` with a "got no reply" warn (`session/lib.rs:1106`), bypassing the `Caller` matching-gate that CLI/tray/SDK all ride. A transiently-unroutable daemon (token-vs-queryable-route race, or a reconnect window) makes an **automation dispatch** — lock/idle/drift/remote-edge → TV command — silently vanish with a warn and no retry, and it can't distinguish "daemon unavailable" from "command rejected." Drop-in fix: one `Caller::new(system.clone())` in `Core`, `.call(&key, body)`; deletes the hand-rolled get/timeout/reply-decode. Low cost; real reliability gain for automation.

### B4 — Minor smells (low value, mostly accept-and-document)

- **tisvlay-gui unused `zenoh` dep** (`crates/tisvlay-gui/Cargo.toml:45`) — no `zenoh::`/`use zenoh` anywhere; likely a port leftover. Verify with `cargo-machete` and remove. Hygiene.
- **`decode_reply` timeout string-sniff** (`wire/lib.rs:1227`) — separates `rpc.timeout` from `unknown` by matching the substring `"Timeout"` in the ReplyError payload; fragile if zenoh changes its synthetic text. No typed alternative in 1.9 — accept and document.
- **CLI quiet-window heuristics** (400/600 ms drains, `cli/main.rs:516,1002`) — could truncate a slow/large fleet snapshot. CLI-only, low.
- **`delete_key` drops the publisher from the map** (`session/lib.rs:1079-1080`) — a re-`publish` re-declares a fresh empty-cache publisher; harmless for delete-then-recreate. One-line note only.
- **Discovery-session token ordering** (`daemon/bridge.rs:358,399`) — token declared after the session id is returned; low value, path is deferred/unconsumed.

## Part C — Explicit non-changes (do NOT "fix" these)

These look like stale-law artifacts but are confirmed correct; recording them so nobody removes them later.

- **`client_config_resilient` (connect-only peer)** — every user is a pure **consumer** (SDK clients, tray mirror links). The stale law only ever blocked *serving*; subscribe/query/hydrate over a connect-only peer was always legal and still is. A "cleaner client-mode-with-retry" alternative would have to re-add `timeout_ms=-1` + `connect_scouted=false` for background-dial-at-open — equal complexity, no gain. **Keep.**
- **The tray dual-link** (resilient-peer consume + supervised-client serve to the user router) — correct by construction: the serve link feeds the GUI, a *third-party* leaf, and the **unchanged** half of the law still requires client→router for third-party fan-out. **Keep.**
- **The `Caller` retry/matching machinery** — rides the still-valid "zero-matching-queryables returns nothing" law; the cleanest 1.9 primitive (Querier + MatchingStatus). **Keep.**
- **The session's two-session structure** (user router + system connect-only peer) — **genuinely required by isolation, not by `single_link`.** A single session bridging both sockets would transitively route the two key spaces (user leaves could read `tv/**`/`manager/**`; the daemon could see `user/**`), defeating the gateway. Even with the law fully corrected, merging breaks isolation. **Doc refinement:** architecture.md conflates two rationales — `single_link` failover (which applies to the *clients* opening two peers) vs isolation (which is what forces the *session's* split). Clarify that the session split stands on isolation alone.

## Triage summary

| # | Change | Law dependence | Cost | Recommend |
|---|---|---|---|---|
| B1 | mirror → cached advanced pub (retires the startup gate + re-put + a subscriber) | corrected serve-side | medium, cross-crate; **pin RESOLVED — viable** | high value; ready to implement (on the feature branch) |
| B2 | fix stale `keys::remote_session` doc comment | corrected serve-side | trivial | do regardless |
| B3 | route `call_tv` through `caller::Caller` | querier-matching (valid) | low | do — real automation-reliability gain |
| A4 | sharpen Law 4 (timestamping = build-time failure) | — | trivial | do |
| C-doc | clarify two-session rationale (isolation, not single_link); Law 5 note; Firesong provenance | — | trivial | do |
| B4 | gui unused dep; decode_reply sniff; CLI windows; delete-publisher note | — | trivial–low | opportunistic |

Nothing here destabilizes the stack; the largest item (B1) removes more than it adds and is gated on one experiment. B2 + B3 + the doc refinements are safe, cheap, and can land immediately.
