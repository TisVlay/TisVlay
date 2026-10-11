# Notification-subsystem follow-up cleanups

Deferred cleanups surfaced by the post-merge review of the
user-configurable desktop-notifications work (the `org.tisvlay.Tray1`
notifications subsystem — see `docs/design.md`). None are bugs — the
feature shipped correct and QA-verified. Each is reuse/idiom work with a
concrete trigger; pick one off when its trigger fires.

(Two review observations are *not* listed here because they're decisions,
not deferred work: reads return the full `NotificationSettings` while an
update carries a `NotificationSettingsPatch` of only the switches it
changes (their rustdoc in `tisvlay-wire`), and the serialized emit-worker /
backoff rationale (`docs/design.md`'s Tray1 section).)

## (Speculative) `PeerClient` trait over the client structs

`DaemonClient` / `SessionClient` / `TrayClient` / `GuiClient` repeat the
same `is_available` (probe `version()`), `version`, `config_path`, and
proxy-build shapes — only the proxy type and error strings differ. A
`trait PeerClient { version, config_path, reload_config, is_available }`
(or a macro over the proxy declarations) could collapse it.

**Trade-offs.** Biggest and least-certain-to-pay-off item here: the four
clients look similar but carry real per-peer differences (bus choice,
availability-probe target, reload semantics — `GuiClient` has no
`reload_config` at all, and `DaemonClient::reload_config` also clears the
proxy cache). The abstraction may cost more in conditionals than the
boilerplate it removes. The sibling `reload_config` / `--config`
boilerplate was already assessed and deliberately left un-extracted
(intentional per-binary divergence — see commit `refactor(gui): dedup
the daemon/session reload-error arms`), so the uniform-interface
precondition this trait would need has not materialized.

**Trigger.** A fifth peer client appears with a genuinely uniform
interface.

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
item above produces (or at minimum add `CacheProperties::No` at that
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
