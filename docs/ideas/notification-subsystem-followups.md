# Notification-subsystem follow-up cleanups

Deferred cleanups surfaced by the post-merge review of the
user-configurable desktop-notifications work (the `tray-notifications`
plan). None are bugs — the feature shipped correct and QA-verified.
These are reuse/idiom/altitude items that were out of that PR's scope
or contested enough to defer a deliberate decision. Grouped here
because they all touch the same subsystem; pick them off independently.

## 1. De-duplicate cross-peer client boilerplate

Three families of copy-paste accumulated as the tray client joined the
daemon/session/gui clients:

- **`reload_config` wrappers.** `DaemonClient`, `SessionClient`, and
  `TrayClient` each carry an identical
  `self.proxy.reload_config().await.context("Failed to reload …")`
  wrapper, and the GUI's `on_reload_config` handler fans out to all
  three with near-identical `if let Err(e) { format!, warn!, push_*_error }`
  arms. The tray arm already drifted — it omits the inline-error banner
  the daemon/session arms push.
- **`--config` arg handling.** `tisvlayd`, `tisvlay-session`, and
  `tisvlay-tray` each repeat the "absolutize an explicit `--config`
  against CWD, else fall back to the XDG default" dance, with the
  absolutize convention documented inline three times. Only the default
  path closure differs per binary.
- **Per-client proxy plumbing.** Each client struct repeats the same
  `is_available` (probe `version()`), `version`, `config_path`, and
  proxy-build shapes — only the proxy type and error string differ.

**Path forward.** Three independent extractions, smallest first:

- `DaemonClient::resolve_tv_name(&self, tv_id) -> String` in core (next
  to `list_tvs`), collapsing the UUID→name lookup currently re-derived
  in `tray/pairing.rs::resolve_tv_name`, `tray/power_cycle.rs`, and
  `tray/state.rs` — with the fresh-pair fallback rule (use the UUID
  when the TV isn't in the list yet) living in one place. This also
  lets `pairing.rs` drop its per-event `list_tvs()` round-trip (the
  same redundancy the second review pass already removed from the
  power-cycle watcher; the two sibling watchers diverge today).
- A `core` helper like `resolve_config_arg(arg: Option<PathBuf>, default: impl FnOnce() -> Result<PathBuf>) -> Result<PathBuf>` centralizing the
  absolutize-or-default rule, leaving only the per-binary default
  closure at each call site.
- A `trait PeerClient { version, config_path, reload_config, is_available }`
  (or a macro over the proxy declarations) to collapse the per-client
  boilerplate, plus a `for client in [...] { reload_or_banner(...) }`
  loop for the GUI fan-out.

**Trade-offs.** Each spans daemon + session + gui + tray, so this wants
its own PR(s), not a notifications follow-on. The `PeerClient` trait is
the biggest and riskiest (touches every client struct); the
`resolve_tv_name` extraction is the cleanest standalone win.

**Trigger.** A fourth peer service appears, the reload-error UX changes
(e.g. toast instead of banner), or a new per-binary config knob lands —
any of which would otherwise mean editing N copies in lockstep.

## 2. Tray `NotificationSettings` vs core `NotificationSettingsBody` — keep two types, but revisit if serde forces it

Three review finders flagged the tray's named-bool `NotificationSettings`
(`tray/config.rs`) as redundant with core's `Option<bool>`
`NotificationSettingsBody` wire dict, proposing the tray store the body
directly and delete `to_body`/`from_body`/`apply` + the round-trip
`Default`.

**Why it was kept.** It is a legitimate DTO-vs-storage split, not
accidental duplication. The wire body needs `Option<bool>` tri-state so
`UpdateNotificationSettings` can be a partial patch ("change only
`tv_wake`"); the on-disk/runtime type wants concrete `bool` ("this
category is on"). Collapsing forces the storage type to carry `Option`
semantics it doesn't want, or strips the wire type's partial-update
ability. The merge logic isn't deleted by a collapse — it moves: with
the body, a missing field means `None` ("unknown"), so `enabled_bitset`
would read it as off unless re-merged against `defaults()` at load.

**Open question to settle first.** Does a *partial* `[notifications]`
TOML table (e.g. only `tv_wake = false`) deserialize today without
per-field `#[serde(default)]`? If hand-editing a partial table is
desired (it's the point of the file) and currently broken or only-works
by luck, that constrains any "simplification" here — and might itself
be the real cleanup.

**Trade-offs.** Collapsing trades a clear two-type contract for fewer
lines, at the cost of muddying partial-update vs full-state semantics.
Current verdict: keep two types. Revisit only if the serde-partial
behavior above turns out to need explicit handling anyway.

## 3. Converge proxy property-cache strategy across clients

`TrayClient` is built with `CacheProperties::No` (its `NotificationSettings`
property is mutable and consumed by an independent `PropertiesChanged`
signal consumer in the GUI — the cached default raced that consumer and
produced the stale-settings-pane bug this subsystem already fixed).
`DaemonClient` / `SessionClient` / `GuiClient` proxies are cached-default;
their const properties dodge staleness via `emits_changed_signal` on the
declaration instead. Three different cache-correctness strategies now
coexist with no shared convention.

**Why deferred.** No current bug — the tray is the only client with a
mutable property consumed by an independent signal consumer, so nothing
else races today. Converging the other clients is out of the
notifications PR's scope.

**Path forward (when it bites).** Either document the rule — "a mutable
property consumed via an independent `PropertiesChanged` signal ⇒
`CacheProperties::No`" — in `docs/design.md` as a breadcrumb, or encode
it in whatever shared `PeerClient` plumbing item 1 produces, so the next
mutable property added to `Session1`/`Gui1` gets the uncached read for
free rather than silently reintroducing the race.

**Trigger.** A second mutable property is added to any peer interface.

## 4. Notification emit worker + `RefreshTrigger` — reviewed, keep as-is

Recorded so future reviewers don't re-litigate it. The tray notifier's
`mpsc` + dedicated-worker design and `RefreshTrigger`'s two-`AtomicBool`
(in_flight + dirty) dedupe were both scrutinized for over-engineering
and found correctly sized:

- The single-worker **serialization is load-bearing**: the failure
  backoff only suppresses a flap-storm because emits run one at a time,
  so `last_failure_at` is stamped before the next request is pulled. A
  fan-out of spawned `show_async` tasks could not guarantee that.
- `RefreshTrigger`'s two-atomic dedupe is the minimum correct shape for
  the wedge bug it documents (awaiting a refresh inline stalls the
  signal-stream drain).

**Verdict.** No change. Removing either would reintroduce a documented
failure mode.
