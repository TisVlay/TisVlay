# One way to mirror a service

Every client keeps a mirror of a service's cached keys and follows the service's lifetime through its liveliness token. Each mirror implements that by hand today, and three bugs found during the TV-controls-the-PC campaign (2026-10-25) were the same mistakes made in different copies. This plan gives the SDK one lifetime-scoped mirror primitive, builds every SDK client and the tray on it, and tests them all against one fake service. It runs as its own train after the campaign's PRs are cut.

## The bugs, and the copies they came from

| Bug | Fixed in | The copy | The mistake |
|---|---|---|---|
| After a resume, the GUI's TV list and system profiles stayed empty | `606b35cb` (train) | the SDK's daemon mirror (`DaemonClient`) | It took a vanished token for a stopped daemon and retired its session, so it ignored the same, still-running daemon once the token returned |
| After a resume, the tray showed the TVs without names | `280b450c` (train) | the tray's own mirror | Subscribed once at startup, so the same daemon's history never replayed, and live samples recreated rows without their identity |
| The tray showed an extra, nameless TV | `b371f514` (train) | the tray's own mirror | Made a TV row from a session-side assignment for a TV the daemon doesn't have |

The resume case: systemd freezes `user.slice` around a sleep, and zenoh's 10 s link lease keeps counting through the kernel's device suspend and resume. A long resume therefore expires every user-side link, and the daemon's token disappears and returns under the same session ([architecture.md](../architecture.md), empirical zenoh laws).

## What exists today

- **Three SDK mirrors** in `crates/tisvlay/src/{lib,mirror}.rs`, each with its own subscriptions and liveliness task:
  - `DaemonClient` / `SystemMirror`: TVs and system profiles, `tisvlay/<site>/tv/**` and `profile/system/*`. Since `606b35cb`, it subscribes per token lifetime and clears on loss.
  - `SessionClient` / `UserMirror`: user profiles, assignments and the stream state, `tisvlay/<site>/user/**`. It subscribes once and never acts on the session's token going away.
  - `TrayClient` / `TrayMirror`: the notification settings. It subscribes once and keeps the settings while the tray is down, by design ("kept while it's down").
- **A fourth mirror in the tray**, which uses the SDK only in its tests: `crates/tisvlay-tray/src/state.rs` (`Mirror`, `TvView`) and the pumps in `lib.rs`. Since `280b450c`, its daemon subscriptions follow the token; its user-socket subscription doesn't.
- **A latent bug of the same class in `SessionClient`:** a profile or assignment deleted while the session service is down, or while the client's link is down over a resume, stays in the mirror, since the delete isn't replayed. After a link drop, nothing re-hydrates what changed in the gap. The GUI shows user profiles from this mirror.

## Decisions to make

1. **The primitive's shape.** A `Lifetime` (working name) owned by each client: given the service's liveliness key, its key expressions and an apply function over a mirror type, it subscribes while the token is present, drops the subscriptions and clears on loss, re-hydrates on return, and emits the client's appeared and disappeared events and its release check (today's `on_liveliness` and `Handshake`). The mirror types keep their apply logic and lose their lifetime code.
2. **What a mirror keeps while its service is down.** The daemon's mirror clears (the GUI shows "offline"). Should the session's mirror clear too, consistently, or keep its profiles visible while the session restarts? The tray's settings are kept on purpose today; does that survive, as a per-mirror "keep while down" flag, or go?
3. **The tray on the SDK.** The tray builds its menu from `DaemonClient` and `SessionClient` and the `StateWatcher` event stream, rather than its own mirror. What it needs beyond today's SDK surface: the per-TV fields `TvView` reads (likely all in `TvInfo`), the connection-transition toasts (from `DaemonStateChanged` on `state/link`), and the volume and backlight echoes its scroll reconcilers observe. The pairing and power-progress toasts already use their own subscriptions; they'd move onto the SDK's progress streams.
4. **Joins across services.** Rows come only from the service that owns them (TVs from the daemon); data from another service is a lookup (the SDK's `UserMirror.assignments` already is). The primitive can't enforce that, so it's a written rule for mirror types, and a conformance case.

## Implementation

- [ ] **The primitive** in the SDK, with `DaemonClient` moved onto it unchanged in behavior.
- [ ] **`SessionClient` and `TrayClient` on it,** with decision 2 applied, fixing the session's latent bug.
- [ ] **The conformance harness:** a fake service (a peer session with cached publishers and a liveliness token) that can drop and return its token under the same session, restart under a new one, delete a record while down, and publish an assignment for a TV it doesn't own. Every client passes the same cases. The two `a_daemon_whose_token_returns_is_mirrored_again` tests (SDK and tray) and `daemon_restart_discards_previous_lifetime_tvs_and_profiles` fold into it.
- [ ] **The tray on the SDK** (decision 3), deleting `state.rs`'s mirror and the tray's pumps; its menu tests keep building `TvTray` from fixtures.
- [ ] **Docs:** architecture.md's client section names the primitive and the join rule; the empirical-law entry points at it.

## Verification

- [ ] The conformance cases pass for every client.
- [ ] On hardware: the GUI and the tray keep their TVs, names and profiles across a resume long enough to drop the links, and across a daemon and a session restart; a profile deleted while the session is down is gone from the GUI when it returns.

## Related

- The link lease: whether to lengthen zenoh's 10 s lease on the Unix-socket links, so a long resume doesn't drop them at all, is a separate decision (offered during the campaign, not taken). With this plan, a drop is harmless either way.
- [docs/ideas/strict-decoding.md](../ideas/strict-decoding.md): the mirrors' apply functions decode leniently today; its open questions cover responses too.
