# Zenoh idiom audit (2026-07-12)

**Status (2026-11-16):** the actionable items landed: the `stream/state` mirror is a cached advanced publisher the daemon hydrates, the key's doc comment says so, the session's TV actions go through `DaemonClient::apply_action`, the timestamping law is sharpened, and the GUI crate's unused `zenoh` dependency is gone. What remains is below: choices that look like mistakes and aren't, and accepted smells. The laws themselves, re-verified by this audit, live in [architecture.md § Empirical zenoh laws](../architecture.md#empirical-zenoh-laws).

The audit checked TisVlay's zenoh usage against zenoh 1.9.0 idioms, after one recorded law (the serve-side "consume-only peer" law) turned out stale and had driven a heavier-than-necessary workaround. Its verdict: the codebase is overwhelmingly idiomatic, and no other law was stale.

## Explicit non-changes (do NOT "fix" these)

These look like stale-law artifacts but are confirmed correct; recording them so nobody removes them later.

- **`client_config_resilient` (connect-only peer)** — every user is a pure **consumer** (SDK clients, tray mirror links). The stale law only ever blocked *serving*; subscribe/query/hydrate over a connect-only peer was always legal and still is. A "cleaner client-mode-with-retry" alternative would have to re-add `timeout_ms=-1` + `connect_scouted=false` for background-dial-at-open — equal complexity, no gain. **Keep.**
- **The tray dual-link** (resilient-peer consume + supervised-client serve to the user router) — correct by construction: the serve link feeds the GUI, a *third-party* leaf, and the **unchanged** half of the law still requires client→router for third-party fan-out. **Keep.**
- **The `Caller` retry/matching machinery** — rides the still-valid "zero-matching-queryables returns nothing" law; the cleanest 1.9 primitive (Querier + MatchingStatus). **Keep.**
- **The session's two-session structure** (user router + system connect-only peer) — **genuinely required by isolation, not by `single_link`.** A single session bridging both sockets would transitively route the two key spaces (user leaves could read `tv/**`/`manager/**`; the daemon could see `user/**`), defeating the gateway. Even with the law fully corrected, merging breaks isolation.

## Accepted smells

- **The timeout reply sniff** — `Caller` (`tisvlay-protocol`'s `caller.rs`) tells `rpc.timeout` from `unknown` by matching the `ReplyError` payload `Timeout`; fragile if zenoh changes its synthetic text. There's no typed alternative.
- **The CLI's quiet windows** (`STATE_QUIET`, `CATALOG_QUIET`) — a drain ends after a quiet spell, which could truncate a slow or large fleet snapshot. CLI-only, low.
- **The session's `delete_key` drops the publisher from its map** — a later publish declares a fresh, empty-cache publisher, which is harmless for delete-then-recreate.
- **Discovery-session token ordering** (the daemon bridge's `alive_discovery` token) — declared after the session id is returned; low value.
