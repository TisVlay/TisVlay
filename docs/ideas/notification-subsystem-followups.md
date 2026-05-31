# Notification-subsystem follow-up cleanups

Deferred cleanups surfaced by the post-merge review of the
user-configurable desktop-notifications work (the `org.tisvlay.Tray1`
notifications subsystem — see `docs/design.md`). None are bugs — the
feature shipped correct and QA-verified. Each is reuse/idiom work with a
concrete trigger; pick one off when its trigger fires.

(Two review observations are *not* listed here because they're decisions,
not deferred work, and already live in `docs/design.md`'s Tray1 section:
the deliberate two-type split between on-disk `NotificationSettings` and
the wire `NotificationSettingsBody`, and the serialized emit-worker /
backoff rationale.)

## Share the per-TV UUID→name lookup

`resolve_tv_name` — `daemon.list_tvs().await → find(uuid) → map(name) →
unwrap_or(uuid)` — is re-derived in three tray sites: `pairing.rs`
(the named helper), `power_cycle.rs::sync_from_daemon` (eager map build),
and `state.rs` (snapshot). The fresh-pair fallback rule (use the UUID
when the TV isn't in the list yet) lives only in `pairing.rs`'s copy, so
a change to it (e.g. show "(unknown TV)" instead of the raw UUID) would
land in only one of the three notification surfaces.

**Path forward.** A single `DaemonClient::resolve_tv_name(&self, tv_id)
-> String` in core, next to `list_tvs`, as the shared home for the
lookup + fallback rule.

**Trade-offs.** Small, self-contained, tray-internal — the cleanest
standalone win of this group. `pairing.rs` does a full `list_tvs()`
(`GetManagedObjects`) per terminal pairing event today; a targeted core
helper could also offer a lighter single-TV `Tv1.Name` read.

**Trigger.** The name-fallback rule changes, or a fourth surface needs
TV-name resolution.

## Factor out `reload_config` + `--config` boilerplate

Two parallel copy-paste families across the peer binaries:

- **`reload_config` wrappers + GUI fan-out.** `DaemonClient`,
  `SessionClient`, and `TrayClient` each carry an identical
  `self.proxy.reload_config().await.context("…")` wrapper, and the GUI's
  `on_reload_config` fans out to all three with near-identical
  `if let Err(e) { format!, warn!, push_*_error }` arms — the tray arm
  already drifted (it omits the inline-error banner the others push).
- **`--config` handling.** `tisvlayd`, `tisvlay-session`, and
  `tisvlay-tray` each repeat "absolutize an explicit `--config` against
  CWD, else fall back to the XDG default," with the absolutize
  convention documented inline three times. Only the default-path
  closure differs per binary.

**Path forward.** A `core` helper like
`resolve_config_arg(arg: Option<PathBuf>, default: impl FnOnce() -> Result<PathBuf>) -> Result<PathBuf>`
for the config side, and a small shared reload-or-banner helper the GUI
loops over for the reload side.

**Trade-offs.** Spans daemon + session + gui + tray, so it wants its own
PR, not a notifications follow-on. Medium size; mechanical.

**Trigger.** The reload-error UX changes, or a new per-binary config knob
lands — either of which would otherwise mean editing N copies in
lockstep.

## (Speculative) `PeerClient` trait over the client structs

`DaemonClient` / `SessionClient` / `TrayClient` / `GuiClient` repeat the
same `is_available` (probe `version()`), `version`, `config_path`, and
proxy-build shapes — only the proxy type and error strings differ. A
`trait PeerClient { version, config_path, reload_config, is_available }`
(or a macro over the proxy declarations) could collapse it.

**Trade-offs.** Biggest and least-certain-to-pay-off of the group: the
four clients look similar but carry real per-peer differences (bus
choice, availability-probe target, reload semantics — `GuiClient` has no
`reload_config` at all). The abstraction may cost more in conditionals
than the boilerplate it removes. Only worth it if the
`reload_config`/`--config` extraction above lands first and a clear
shared shape emerges from it.

**Trigger.** A fifth peer client appears, or the extraction above reveals
a genuinely uniform interface.

## Converge proxy property-cache strategy across clients

`TrayClient` is built with `CacheProperties::No` (its mutable
`NotificationSettings` property is consumed by an independent
`PropertiesChanged` consumer in the GUI — the cached default raced that
consumer and produced the stale-settings-pane bug this subsystem fixed).
`DaemonClient` / `SessionClient` / `GuiClient` proxies are cached-default.
The *rule* — "a mutable property consumed via an independent
`PropertiesChanged` signal must be read uncached" — is documented in
`docs/design.md`'s Tray1 correctness lessons; what's deferred is
*enforcing* it across the other clients.

**Path forward.** When a second mutable property lands on any peer
interface, encode the rule in whatever shared `PeerClient` plumbing the
items above produce (or at minimum add `CacheProperties::No` at that
site), rather than rediscovering the race.

**Trade-offs.** Pure YAGNI today — no other client currently has a
mutable property consumed by an independent signal consumer, so there's
no live bug. Listed only so the next mutable-property author finds the
rule.

**Trigger.** A second mutable property is added to any peer interface.

## Open question: partial `[notifications]` TOML

Worth a 5-minute check independent of the items above: does a *partial*
`[notifications]` table in `tray.toml` (e.g. only `tv_wake = false`,
omitting the other keys) deserialize today, or does it require every key
present? Hand-editing a partial table is the point of the file. If it
only works by luck (or not at all) without per-field `#[serde(default)]`,
that's a real correctness gap to fix — and it directly informs whether
the two-type split (see design.md) ever needs revisiting.
