# One way to mirror a service

Every client keeps a mirror of a service's cached keys and follows the service's lifetime through its liveliness token. Each mirror implements that by hand today, and three bugs found during the TV-controls-the-PC campaign (2026-10-25) were the same mistakes made in different copies. This plan gives the SDK one lifetime-scoped mirror primitive, builds every SDK client and the tray on it, and tests them all against one fake service. It runs as its own train after the campaign's PRs (#216, #217).

## The bugs, and the copies they came from

| Bug | Fixed in | The copy | The mistake |
|---|---|---|---|
| After a resume, the GUI's TV list and system profiles stayed empty | `606b35cb` (#216) | the SDK's daemon mirror (`DaemonClient`) | It took a vanished token for a stopped daemon and retired its session, so it ignored the same, still-running daemon once the token returned |
| After a resume, the tray showed the TVs without names | `280b450c` (#216) | the tray's own mirror | Subscribed once at startup, so the same daemon's history never replayed, and live samples recreated rows without their identity |
| The tray showed an extra, nameless TV | `b371f514` (#216) | the tray's own mirror | Made a TV row from a session-side assignment for a TV the daemon doesn't have |

The resume case: systemd freezes `user.slice` around a sleep, and zenoh's 10 s link lease keeps counting through the kernel's device suspend and resume. A long resume therefore expires every user-side link, and the daemon's token disappears and returns under the same session ([architecture.md](../architecture.md), empirical zenoh laws).

## What exists today

- **Three SDK mirrors** in `crates/tisvlay/src/{lib,mirror}.rs`, each with its own subscriptions and liveliness task:
  - `DaemonClient` / `SystemMirror`: TVs and system profiles, `tisvlay/<site>/tv/**` and `profile/system/*`. Since #216 it subscribes per token lifetime (`mirror_daemon`), clears on loss, subscribes before it reports the daemon available, and aborts a lifetime's started tasks when a later subscribe fails.
  - `SessionClient` / `UserMirror`: user profiles, assignments and the stream state, `tisvlay/<site>/user/**`. It subscribes once and never acts on the session's token going away.
  - `TrayClient` / `TrayMirror`: the notification settings. It subscribes once and keeps the settings while the tray is down, by design ("kept while it's down").
- **A fourth mirror in the tray**, which uses the SDK only in its tests: `crates/tisvlay-tray/src/state.rs` (`Mirror`, `TvView`) and the pumps in `lib.rs`. Its daemon subscriptions follow the token (its own `mirror_daemon`); its user-socket subscription doesn't.
- **A latent bug of the same class in `SessionClient`:** a profile or assignment deleted while the session service is down, or while the client's link is down over a resume, stays in the mirror, since the delete isn't replayed. After a link drop, nothing re-hydrates what changed in the gap. The GUI shows user profiles from this mirror.
- **Recovery is half wired:** the daemon's publishers send miss-detection heartbeats (`crates/tisvlay-daemon/src/bridge.rs`, `sample_miss_detection(MissDetectionConfig::default().heartbeat(MISS_HEARTBEAT))`), but no client subscriber enables zenoh-ext's `recovery`, so a sample lost within a lifetime (not across a link drop) is never retransmitted.

## Decisions (2026-10-31)

1. **The shape: a `Mirror` trait driven by a generic `ServiceMirror<M>`.** The precedent is tokio-util's `FramedRead<T, D: Decoder>`: a generic driver owns the I/O, and a trait carries the stateful per-item logic. Here the trait is the mirror type's (`type Event; fn apply(&mut self, site, &Sample) -> Option<Self::Event>; fn clear(&mut self)`), and `ServiceMirror<M>` owns the service's liveliness subscriber, the per-lifetime key subscriptions, the event broadcast and the release check (today's `on_liveliness` and `Handshake`). Closures were the alternative; two callbacks sharing one state (apply and clear) read worse than a trait. The mirror stays behind a std `Mutex`, never held across an `.await`.
   - **Why a lifetime layer of our own:** zenoh-ext's `AdvancedSubscriber` detects late publishers and recovers misses per publisher, but has no notion of a service going away, and its per-publisher sequencing drops history it has already delivered. A mirror that clears on loss must therefore re-subscribe on return to get the history again, which is the `DaemonClient` pattern.
   - **Recovery, too:** the primitive's subscriptions enable `RecoveryConfig::default().heartbeat()`, pairing with the daemon's existing heartbeats, so a sample lost within a lifetime is retransmitted from the publisher's cache. Heartbeat, not `periodic_queries`: nothing here publishes periodically, and zenoh's #2666 fixed a runtime-context panic in the periodic-queries path.
2. **Every mirror clears while its service is down,** uniformly: the daemon's, the session's and the tray settings'. Clients show the service as offline rather than stale state that edits would fail against. This retires the tray settings' "kept while it's down".
3. **The tray moves onto the SDK in this train,** as its second half. The tray builds its menu from `DaemonClient`, `SessionClient` and the `StateWatcher` event stream, and `state.rs`'s mirror and the tray's pumps go. What it needs beyond today's SDK surface: the per-TV fields `TvView` reads (likely all in `TvInfo`), the connection-transition toasts (from `DaemonStateChanged` on `state/link`), and the volume and backlight echoes its scroll reconcilers observe. The pairing and power-progress toasts move onto the SDK's progress streams.
4. **Joins across services:** rows come only from the service that owns them (TVs from the daemon); another service's data is a lookup (the SDK's `UserMirror.assignments` already is). The primitive can't enforce it, so it's a written rule for mirror types in architecture.md and a conformance case.

## Implementation

A linear train, a commit per step.

- [ ] **1. The primitive, with `DaemonClient` on it.** `Mirror` and `ServiceMirror<M>` in the SDK; `SystemMirror` implements `Mirror`; `DaemonClient` keeps its public surface and behavior, and its liveliness task and `mirror_daemon` go. Recovery on from the start.
- [ ] **2. `SessionClient` and `TrayClient` on it.** `UserMirror` and `TrayMirror` implement `Mirror` and clear on loss (decision 2), fixing the session's latent bug. The SDK docs say what each client shows while its service is down.
- [ ] **3. The conformance harness.** A fake service (a peer session with cached publishers that send heartbeats and a liveliness token) that can drop and return its token under the same session, restart under a new one, delete a record while down, lose a sample within a lifetime, and publish an assignment for a TV it doesn't own. Every client passes the same cases. The two `a_daemon_whose_token_returns_is_mirrored_again` tests (SDK and tray) and `daemon_restart_discards_previous_lifetime_tvs_and_profiles` fold into it.
- [ ] **4. The tray on the SDK** (decision 3), deleting `state.rs`'s mirror and the tray's pumps; its menu tests keep building `TvTray` from fixtures. Any SDK surface the tray needs lands here, before the tray uses it.
- [ ] **5. Docs:** architecture.md's client section names the primitive and the join rule; the empirical-law entry on link drops points at it; the SDK's crate docs say what a client shows while a service is down.

## Verification

- [ ] The conformance cases pass for every client.
- [ ] On hardware: the GUI and the tray keep their TVs, names and profiles across a resume long enough to drop the links, and across a daemon and a session restart; a profile deleted while the session is down is gone from the GUI when it returns; the tray's settings read offline while the tray is down and come back with it.

## Related

- The link lease: whether to lengthen zenoh's 10 s lease on the Unix-socket links, so a long resume doesn't drop them at all, is a separate decision (offered during the campaign, not taken). With this plan, a drop is harmless either way.
- [docs/ideas/strict-decoding.md](../ideas/strict-decoding.md): the mirrors' apply functions decode leniently today; its open questions cover responses too.
