# Plan 6 — Edit TV properties from the GUI

> **Status:** Draft v2 — all open questions resolved. UUID
> v4 identity (matches NetworkManager); `SetTvOption`
> retired in favor of `UpdateTvConfig`; wire format stays
> JSON-string (option b) with the `a{sv}` migration
> deliberately deferred until feature-completeness reduces
> surface churn. Builds on plan5's schema architecture
> (`config_schema` + `FieldVisibility`) which gave us the
> declarative metadata to drive an edit form for free.
>
> | Decision | Status | Section |
> |---|---|---|
> | UUID v4 identity model + rename `Tv1.Id` → `Tv1.Uuid` | ✅ resolved | Identity model |
> | Drop `Tv1.Address` cleanly; no scalar replacement | ✅ resolved | Drop `Tv1.Address` |
> | Atomic batch edit; pencil-toggle UX | ✅ resolved | UX shape |
> | Accept config + reflect connection status on side-effect failure | ✅ resolved | Atomicity |
> | Drop `SetTvOption`; route everything through `UpdateTvConfig` | ✅ resolved | `SetTvOption` retirement |
> | Edit-mode scope: INFO-only modal | ✅ resolved | (chat) |
> | Validation timing: save-button-gated like wizard | ✅ resolved | (chat) |
> | No config-load migration; regenerate sample configs directly | ✅ resolved | Identity model |
> | Atomic config writes via tempfile + rename | ✅ resolved | Identity model |
> | Wire format for `UpdateTvConfig`: JSON-string (defer `a{sv}` migration) | ✅ resolved | Open questions / Deferred |

## Bug recap (motivation)

Users currently have no way to update a TV's properties after
pairing. Common needs that fail today:

- TV moves to a new IP (DHCP lease change, network reorg).
- MAC was missing at pair time and the user wants to add it for
  Wake-on-LAN (the original plan5 motivation, now solved at pair
  time but also needed for already-paired TVs).
- TV gets a new physical location and the friendly name should
  update.
- User wants to flip a setting and re-pair isn't worth the
  ceremony.

The schema work in plan5 gave us the data model
(`config_schema` declares which fields exist with their
`visibility`). What's missing is the user-side path to edit
those fields and the daemon-side path to apply changes
atomically with their side effects.

## Scope

| # | Item | Effort |
|---|---|---|
| 1 | Stop leaking vendor-specific IP into `Tv1.Address` | medium |
| 2 | Daemon `Manager1.UpdateTvConfig(tv_id, json_patch)` + atomicity | medium |
| 3 | Daemon `Manager1.GetTvConfig(tv_id)` for prefilling the edit form | small |
| 4 | Extract `SchemaForm` + validation lifecycle from the wizard | small-medium |
| 5 | GUI edit-mode toggle in the TVs detail INFO group | medium |
| 6 | Side-effect polish: IP-change reconnect flow, MAC `SupportsWake` refresh | small |
| 7 | Rename / ID-collision UX | small (depends on Q1) |

## Core architectural decisions

### Identity model: UUID + display name ✅ resolved (was Q1)

Most mature CRUD systems separate identity from display:

| System | Identity | Display | Renameable? |
|---|---|---|---|
| NetworkManager | UUID | name | yes; CLI accepts both |
| Slack / Discord | channel ID | name | yes; mentions resolve via ID |
| Notion | page ID | title | yes; URL slug adapts |
| macOS / KDE address book | record ID | display name | yes; references stable |
| Linux user accounts | uid | login name + GECOS | login renameable; uid not |
| Git remotes | name (no separate ID) | same | yes via explicit `git remote rename` |

The dominant pattern: **immutable internal identifier + freely
renameable display field**. Git's the exception (no separate
identity layer) and pays for it with a dedicated rename command
that touches every reference.

**Resolution:** migrate `tv.config.id` from "sanitized name" to
**UUID** (RFC 4122). Reasons:

1. **D-Bus path correctness.** D-Bus path components match
   `[A-Za-z0-9_]+` per spec — no hyphens, no Unicode. Sanitized
   names from arbitrary user input ("客厅电视", "Sala de estar")
   collapse to nothing useful or awkward placeholders. UUID
   sidesteps the question.
2. **i18n correctness.** Same root cause: the sanitized-name
   approach silently restricts users to ASCII names. UUIDs free
   the `name` field to be anything Unicode permits.
3. **Stable cross-references.** Profile assignments
   (`profile.tv.<key>`), the daemon's `IndexMap<id, …>`, scripts,
   cron jobs — all keep working through renames.
4. **NetworkManager precedent.** Same problem space, same
   answer.

**Concrete shape:**

- Generate a UUID at TV creation; store as `tv.config.uuid`.
  Use **UUID v4** (random) — matches NetworkManager's
  `nm_uuid_generate_random()` (`src/libnm-glib-aux/nm-uuid.c`),
  matches the `uuid` crate maintainers' guidance for stable
  opaque keys, no clock dependency, no distributed-ordering
  need. v7 (timestamp-sortable, RFC 9562) buys nothing here
  and adds a wall-clock footgun at first-pair time. v5
  (namespace+name) would either collide or churn against MAC
  randomization.
- Cargo: `uuid = { version = "1", features = ["v4", "serde"] }`.
- TOML/JSON storage uses canonical hyphenated form
  (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`).
- D-Bus path uses underscores (D-Bus path components match
  `[A-Za-z0-9_]+` per spec). Conversion happens at
  path-construction sites only — the existing `tv_path()`
  helper in `tisvlay-core::dbus_paths` already does
  alphanumeric-or-underscore sanitization, so it
  hyphen-to-underscore-converts a UUID for free.
- Cross-references in config (profile assignments,
  `primary_input` keying) use UUID.
- Rename `Tv1.Id` → `Tv1.Uuid` to make the abstraction
  unambiguous. `Tv1.Name` stays as user-facing display.
- CLI accepts `name | uuid` for lookups (NetworkManager's
  `nmcli connection up <name|uuid>` pattern). Names primary —
  `tisvlay tvs power-on Office` still works — UUID is the
  ambiguity-breaker.

**No migration path needed.** The project has no live external
users; sample configs in `docs/configs/` get regenerated as
part of the phase. Existing developer-machine configs require
re-pairing or a manual edit to add `uuid` fields. This shrinks
Phase 8.1's scope substantially — no detect-and-rewrite logic
in the daemon's config loader.

**Atomic config persistence.** When the daemon does need to
write the config back (e.g. `pair_tv` adds a new entry, or
`UpdateTvConfig` mutates one), use write-to-tempfile +
`rename(2)` for atomic replacement. Standard pattern; avoids
"crash mid-write left the config half-rewritten" reasoning
about partial states.

**No collision problem.** Renames update only `name`; UUID
stays. Two TVs with the same name is fine (UUIDs disambiguate);
the GUI/CLI surfaces a warning if a name is ambiguous.

For advanced users wanting to manually edit IDs: they already
own the config file. Editing `~/.config/tisvlay/config.toml` to
swap `uuid` values is supported the same way it always has
been — daemon picks up the new value on `ReloadConfig`.

### Drop `Tv1.Address` entirely ✅ resolved (was Q2)

`Tv1.Address` exposes the LG-specific IP as a top-level
cross-vendor property. Fine when LG was the only backend; now
that schema-driven `Tv1.VendorInfo` carries the same IP under
its proper label, `Tv1.Address` is redundant — and worse, it's a
shape future non-IP backends (USB-CEC at `/dev/ttyUSB0`,
Bluetooth at `XX:XX:XX:XX:XX:XX`) would have to shoehorn into.

**Resolution:** drop `Tv1.Address` cleanly. **No replacement
scalar** — no `Tv1.URI`, no `Tv1.Locator`, no `Tv1.Description`.
GUI's spatial hint reads `Tv1.VendorInfo[0]` (first entry) for
the "Office — 192.168.1.170" header treatment. Backend orders
its `config_schema` so the most informative field comes first;
LG's order is `[ip, mac, client_key, scheme]` — `ip` is first
in vendor_info → still surfaces as the spatial hint.

**Why no replacement scalar:** any top-level `Tv1.X: s` property
forces a "vendor-agnostic naming" decision (URI? Locator?
Description?) that's always slightly wrong for some backend.
Convention via `vendor_info` ordering is implicit but
vendor-honest — backend decides what's most informative for *its*
transport and puts it first.

If "implicit ordering" turns out fragile in practice, the
explicit alternative is a `primary: bool` flag on `PairingField`
(adds one more axis to a struct that's already at 13 fields, but
unambiguous). Defer until a backend hits a case the convention
doesn't cover.

**`DiscoveredTv.address`** stays — it's a string used pre-pair
when no `vendor_info` exists yet (discovery results haven't
been promoted to a paired TV). Field name is slightly
misleading (some future backend's `address` might not be an IP)
but it's clearly a String not an `IpAddr`; consumers treat it
as opaque display text. Renaming for cosmetics isn't worth the
churn.

**Cleanup touches:**

- `TvService.address` cached field — drop.
- `Tv1.Address` D-Bus property + proxy — drop.
- `TvInfo.address` in `dbus_client.rs` — drop.
- `TvConnector::display_address(vendor_config) -> String` —
  drop. Daemon's log-formatter switches to "first entry of
  `display_info(vendor_config)`" when it needs a one-liner.
- Every read site (TVs pane header, tray menu) — switch to
  `vendor_info` lookups, or stay on `DiscoveredTv.address` for
  the pre-pair case.

### Atomicity & failure modes (resolved Q4)

User-confirmed: edits accept successfully even if the new
config is unreachable. Connection status reflects the outcome
so the user gets responsive feedback. The daemon's
`update_tv_config` flow:

1. Validate the patch against `config_schema` (rejects Secret /
   Diagnostic field updates; canonical-format checks via
   `PairingField::validate`).
2. Persist to disk (so a crash mid-update doesn't leave the
   in-memory config out of sync).
3. Update `tv.config` under the per-TV lock.
4. Drop the existing connection (if vendor-config fields like
   `ip` changed, since the old connection is talking to the old
   address).
5. Re-register D-Bus interfaces (so cached properties like
   `SupportsWake` refresh against the new config).
6. Emit `Tv1.PropertiesChanged` and `Tv1.VendorInfoChanged`.
7. Trigger a reconnect attempt — same path as `Tv1.Reconnect`.
   Failure is non-fatal; surfaces as `Connected = false` plus
   the existing inline-error mechanism if user-initiated.

This means an "edit IP to wrong address" sequence:

- User submits edit → daemon accepts and persists.
- Reconnect fails → `Tv1.Connected = false`, status indicator
  goes red, INFO pane shows the new (wrong) IP.
- User notices, edits IP back to the correct value.
- Daemon accepts, reconnects, all green.

No data loss; user always recoverable.

### UX shape (resolved Q2 from chat)

User-confirmed: edit-pencil icon in the INFO group's title
bar. Click → writable rows become text inputs (per-row
validation lifecycle — same component as the wizard). The
pencil swaps for Cancel / Save buttons. Submit collects all
edits into one batch patch and calls `Manager1.UpdateTvConfig`.

Mirrors the address-book-contact "edit mode" pattern.

Read-only rows (TV ID, manufacturer, model, serial, DRM
connector, scheme) stay as labels even in edit mode.

## D-Bus surface

### `Manager1.UpdateTvConfig(tv_id: s, json_patch: s) -> ()`

Takes a JSON object whose keys are field names from the TV's
`config_schema` (filtered to `UserInput && !secret`). Top-level
fields (`name`, `managed`, `system_profile`, `primary_input`)
also accepted in the same patch — atomicity covers both.

Validation:
- Reject any field not in the schema's writable projection.
- Reject any `Secret` or `Diagnostic` field name (defense in
  depth — server doesn't trust client to filter).
- Per-field format check via `PairingField::validate`.
- Cross-field validation if needed (none for v1).

Error: `ConfigError::InvalidVendorConfig { field, reason }` —
same shape as `AddTv` so the GUI's existing inline-error
routing works unchanged.

### `Manager1.GetTvConfig(tv_id: s) -> s`

Returns the current vendor + top-level config as JSON, with
`Secret` fields stripped. Edit form prefills from this.

(Could alternatively reuse the `Tv1.VendorInfo` property + the
existing top-level properties, but a single RPC is simpler for
the modal/edit-mode flow.)

### `SetTvOption` retirement (research, leaning resolved)

`Manager1.SetTvOption(tv_id, key, value)` — the existing
stringly-typed dispatcher — has accumulated meaningful design
churn. Worth tracing before deciding what to do with it.

**Git history of `SetTvOption`:**

- **Commit `2e88717` (Phase 5A)** introduced it for the
  `managed` flag. Initial keys: `hdmi_input`,
  `only_when_active`, `managed`, `name`. The shape — `(tv_id,
  key, value)` strings — was chosen for parity with how
  `tisvlay-cli` dispatched config edits at the time.
- **Subsequent commits** drifted the supported-keys list:
  - `hdmi_input` → `primary_input` (rename, when input
    handling generalized).
  - `only_when_active` → dropped (replaced by
    `automate_when` resolved through profiles).
  - `profile.system` added (per-TV profile assignment).
  - Each rename / addition required: a new dispatch arm in
    `apply_tv_option`, a doc-comment update on the D-Bus
    method, and a synchronization between `tv.config` writes
    and disk persistence.
- The doc comment (`Supported keys: …`) drifted out of date
  twice in history; clients reading the introspection saw
  fields that no longer existed. No machine-checkable contract
  between the daemon's accepted-keys list and what was
  introspected.

The pattern: **every editable field is a per-commit decision
that touches the dispatch + doc + tests**, with nothing
schema-driven.

**D-Bus / freedesktop precedent for batch-vs-per-field:**

| Project | Pattern | Notes |
|---|---|---|
| **NetworkManager** `Settings.Connection.Update(settings: a{sa{sv}})` | atomic blob | Settings is a dict-of-dicts (vendor groups under outer key). Replaces full settings, OR `UpdateUnsaved` for in-memory only. **No per-property setter exists.** |
| **systemd** `Manager.SetUnitProperties(name, runtime: b, properties: a(sv))` | batch list | Hybrid: explicit `(name, variant)` tuples, no JSON. Atomicity via the array. **No `SetUnitProperty` singular variant.** |
| **BlueZ** `Adapter1`, `Device1` | per-property via `org.freedesktop.DBus.Properties.Set` | Standard. No batch update. Each property auto-emits `PropertiesChanged` on Set. |
| **UPower** `Device` | per-property via Properties.Set | Same as BlueZ. |
| **GNOME / GSettings** | per-key `Set(key, variant)` | Settings backend; not directly comparable but same shape. |

**The split that matters:** projects that treat their state as
a structured object with vendor-extensible fields (NetworkManager
connections, systemd unit properties) chose **batch**. Projects
that treat their state as a flat property bag of independent
scalars (BlueZ, UPower) chose **per-property via standard
Properties.Set**.

tisvlay's TV config is the structured-object case: vendor
fields plus top-level fields, with atomicity required for the
GUI's edit-pencil flow. So the batch precedent applies.

**tisvlay's existing convention for *live state*:** typed
methods, not Properties.Set. Per commit `87714ad` (`refactor(daemon):
writable state via methods, properties read-only`), live state
(`Volume`, `Backlight`, `Screen`, `Mute`) uses
`SetVolume(y)`/`SetBacklight(y)`/`SetScreen(b)`/`SetMuted(b)`
methods with read-only properties as cached mirrors. The
commit message explicitly cites UPower / GNOME settings / BlueZ
/ NetworkManager precedent and zbus's screen-brightness
example, plus a concrete reason: typed methods avoid the
auto-emit-on-Set vs. state_forwarder race that motivated
earlier "drop pre-cache" workarounds.

That decision was deliberate and well-grounded. It tells us
**tisvlay's standing convention is "typed methods, not
Properties.Set"** — but it was made for *live runtime state*,
where the property count is bounded (Volume / Backlight /
Screen / Mute / current input switch), and where each setter
has a clear typed signature.

**Config state is a different shape:**

- Vendor-extensible (LG: ip, mac; future Samsung: api_key;
  USB-CEC: serial_port, baud_rate).
- Atomicity matters (edit pencil saves all-or-nothing).
- Schema-driven (we already declare it via
  `TvConnector::config_schema`).

Typed methods for config state would mean per-vendor
sub-interfaces (`Tv1.Lg.SetIp`, `Tv1.Lg.SetMac`) — leaks
vendor semantics back into the D-Bus surface that we just
spent plan5 + plan6 cleaning up. Per-property `Properties.Set`
loses the atomicity. Stringly-typed `SetTvOption` is the
churn-prone shape we have.

**Leading recommendation: drop `SetTvOption`, route everything
through `UpdateTvConfig(tv_id, json_patch)`.**

Rationale:

1. **Symmetry with profile CRUD.** plan.md's M3.5 already
   commits to `Manager1.UpdateSystemProfile(name, json)` and
   `Session1.UpdateUserProfile(name, json)`. Both blob-JSON.
   TV config matching that shape gives one CRUD pattern across
   the daemon.
2. **Schema-driven.** Adding a new editable field requires
   updating only `config_schema` — no D-Bus method changes, no
   dispatch-arm additions. The historical churn around
   `SetTvOption` keys becomes structurally impossible.
3. **NetworkManager precedent** for vendor-extensible
   structured state.
4. **Atomicity.** GUI edit-pencil flow saves multiple fields
   together; per-field calls don't compose cleanly.

**Trade-off:** loses the introspection ergonomics of
"`busctl introspect` shows me each setter's signature." Mitigated
by `Manager1.GetPairingSchema(vendor)` already exposing the
schema as JSON for tooling that wants typed metadata; the
analogous `GetTvConfigSchema(tv_id)` (or just reusing the
existing endpoint) covers the edit case.

**Live state (Volume / Backlight / Screen / Mute) keeps its
typed-method pattern unchanged** — different problem space,
the 87714ad rationale still applies.

**Status: ✅ resolved.** Research pass confirmed the batch
recommendation:

- **ModemManager** (the closest tisvlay analog — vendor-extensible
  cellular modems with per-vendor config) uses **batch `a{sv}`**
  pervasively: `Modem.Simple.Connect(properties: a{sv})`,
  `Modem3gpp.ProfileManager.Set(properties: a{sv})`,
  `Modem3gpp.SetInitialEpsBearerSettings(a{sv})`. No use of
  `org.freedesktop.DBus.Properties.Set` for vendor-extensible
  config; properties are read-only. ProfileManager is the
  closest analog to TV config and uses dict-of-variants.
- **zbus dynamic `Properties.Set`** is technically possible
  via the low-level `zbus::object_server::Interface` trait
  (which exposes `set(property_name, value, ...)` with
  string-name dispatch), but zbus docs explicitly discourage
  manual `Interface` impls — the `#[interface]` macro is the
  supported path. No examples in the zbus repo of schema-driven
  dynamic property dispatch. Confirmed off-idiom.

The ModemManager research **also surfaces a wire-format
question**: freedesktop precedent is `a{sv}` (D-Bus variant
dict), not JSON-string. tisvlay's existing `AddTv(config_json:
s)` chose JSON-string deliberately (plan5 v3 Q1) for
consistency across the existing surface. Open as **❓v2 Q6**
in the post-research questions section below.

## Side effects per field

| Field | Effect |
|---|---|
| `name` | `Tv1.Name` PropertiesChanged. UUID stays (per Q1 resolution). Tray menu refreshes. Trivial. |
| `system_profile` | Re-resolve any session bindings. Currently wired via `SetTvOption`; routes through `UpdateTvConfig` after Q3 retirement. |
| `primary_input` | Affects `Tv1.Automated` recomputation. Same routing change as above. |
| `managed` | Lifecycle automation respects new flag. Same routing change. |
| `ip` (vendor) | Connection drops. Daemon reconnects with new IP. NIC route re-resolves. Subscriptions re-bring-up (parallelized — fast). |
| `mac` (vendor) | No connection break. `Tv1.Power.SupportsWake` recomputes. Power-on button enables/disables. |

Only `ip` has nontrivial coordination. The flow already exists
inside `tv_manager::reconnect`; we just need to wire the
edit-applied → drop-and-reconnect path.

## Refactoring needs

1. **Migrate `id` → `uuid`.** New `tv.config.uuid` field
   (RFC 4122). D-Bus path becomes
   `/org/tisvlay/Tv/<uuid_underscored>`. Cross-references in
   config (profile assignments, primary_input keying) use
   UUID. Rename `Tv1.Id` → `Tv1.Uuid`. One-shot config-load
   migration for existing sanitized-name IDs (generate UUID,
   write back, log). Touches every site that handles
   `tv_id` strings: pairing wizard, CLI lookups, tray menu,
   GUI selection state, profile assignment, D-Bus paths.

2. **Drop `Tv1.Address` and `TvConnector::display_address`.**
   Per Q2 above. Touches:
   - `TvService.address` cached field
   - `Tv1.Address` D-Bus property + proxy
   - `TvInfo.address` field
   - Every read site (tray, GUI header) — switch to
     `vendor_info[0]` for the spatial hint, or stay on
     `DiscoveredTv.address` for the pre-pair case
   - `TvConnector::display_address` trait method — drop.
     Replace internal log-formatter callers with "first
     entry of `display_info(vendor_config)`".

3. **Drop `Manager1.SetTvOption`.** Per Q3. All callers
   (tray, CLI, GUI) switch to `Manager1.UpdateTvConfig`. The
   existing `apply_tv_option` helper in `tv_manager.rs` stays
   internal — `update_tv_config` calls it for the top-level
   fields it knows about (name, managed, system_profile,
   primary_input).

4. **Hoist `SchemaForm` from `pairing_wizard.slint`.** The
   `for f in schema-fields: FieldRenderer { ... }` loop, the
   per-field validation lifecycle, the "Pair" button gating —
   all of it works for the edit pane unchanged. Extract to a
   shared component so wizard and edit pane both consume it.

5. **Hoist Rust validation helpers.** `autofill_schema`,
   `validate_value`, `compute_schema_valid`,
   `build_config_json_from_schema`,
   `parse_field_type` currently live in
   `tisvlay-gui/src/main.rs`. Move to a shared module
   (`tisvlay-gui/src/schema_form.rs`?) so the edit flow
   doesn't duplicate them.

6. **`TvManager::update_tv_config(tv_id, json_patch)`** —
   new method coordinating: validate against `config_schema`,
   apply (top-level via `apply_tv_option`, vendor via
   merge-into-`tv.config.vendor`), persist to disk,
   re-register D-Bus interfaces, trigger reconnect if vendor
   fields changed.

7. **`TvManager::get_tv_config_json(tv_id)`** — assembles the
   prefill payload by walking `config_schema` and reading
   current values from `tv.config`. Strips `Secret`.

## Phases

Suggested order, smallest reviewable units first:

- **Phase 8.0 — Drop `Tv1.Address` + `display_address`.** Pure
  refactor, no UX change. Everywhere that displayed `address`
  switches to reading `vendor_info` (or stays on
  `DiscoveredTv.address` for the pre-pair case). Drops the
  trait method too. Frees the next phases from working
  around the leaky abstraction.
- **Phase 8.1 — UUID identity.** Introduce `tv.config.uuid`
  (v4, generated at pair time via the `uuid` crate), switch
  D-Bus paths, rename `Tv1.Id` → `Tv1.Uuid`. CLI gains
  name/UUID dual-lookup. Sample configs in `docs/configs/`
  regenerated as part of the phase — no daemon-side migration
  logic since we have no live external users. Atomic config
  writes (tempfile + rename) land here too. Touches every
  `tv_id` string site (pairing wizard, tray menu, profile
  assignment, GUI selection state) but the per-site changes
  are mostly mechanical.
- **Phase 8.2 — Extract `SchemaForm` + Rust validation
  helpers.** Pure refactor. Wizard keeps working unchanged;
  shared infrastructure ready for edit pane.
- **Phase 8.3 — Daemon `UpdateTvConfig` + `GetTvConfig`.**
  Adds the new methods with server-side schema validation +
  atomicity + side-effect handling. Test via `busctl` before
  any GUI work.
- **Phase 8.4 — Drop `SetTvOption`, migrate callers.** Tray,
  CLI, GUI all switch to `UpdateTvConfig`. Daemon removes
  the dispatcher. Reviewable as a single coordinated change
  since we have no backward-compat constraint.
- **Phase 8.5 — GUI edit-mode toggle.** Pencil icon in INFO
  group → row inputs + Cancel/Save. Reuses Phase 8.2
  components. Wired to Phase 8.3 RPCs.
- **Phase 8.6 — Side-effect polish.** Reconnect-on-IP-change
  flow, `SupportsWake` refresh, status indicator surfaces a
  brief "applying…" / "connection lost" state during
  edit-with-side-effect.
- **Phase 8.7 (optional)** — CLI `tisvlay tvs <name|uuid>
  set <field>=<value>` shorthand wrapping `UpdateTvConfig`.
  Mirrors NetworkManager's `nmcli connection modify`.

## Open questions

**v1 (resolved):**

- **✅ Q1** — UUID v4 identity model. Rename `Tv1.Id` →
  `Tv1.Uuid`. No config-load migration; sample configs
  updated directly. See "Identity model" section.
- **✅ Q2** — Drop `Tv1.Address` cleanly, no replacement
  scalar. GUI reads `vendor_info[0]` for the spatial hint.
  `DiscoveredTv.address` stays as-is.
- **✅ Q3** — Drop `SetTvOption`; route everything through
  `UpdateTvConfig`. ModemManager + zbus research confirmed
  batch is the right pattern. See "SetTvOption retirement".
- **✅ Q4** — Edit-mode covers Name + vendor UserInput
  fields only. Inline controls (System profile, Primary
  input, Managed) stay where they are.
- **✅ Q5** — Save-button-gated validation, same as the
  wizard's "Pair" button gating. Server-side errors surface
  inline on the originating field for cases the client
  validator can't catch.

**v2 (resolved):**

- **✅ Q6** — `UpdateTvConfig(tv_id: s, json: s) -> ()` (option
  **b**). Keep the JSON-string convention established by
  `AddTv` (plan5 v3 Q1), `GetPairingSchema`, and
  `ListSystemProfiles`. Defer the all-in `a{sv}` migration
  (option c) until feature-completeness reduces the rate of
  surface churn — the freedesktop-convention benefit grows
  with introspection consumers and multi-vendor pressure,
  neither of which exists today. See "Deferred to a future
  PR" below.

  Brief rationale for the deferral over committing to (c)
  now: `a{sv}` is the architecturally correct end-state, but
  none of its concrete benefits (introspection by external
  tooling, native bus-level type checks, multi-vendor symmetry
  pressure) materialize at the project's current stage. The
  cost of locking in (c) now is migrating `AddTv` callers
  during peak-development churn; the cost of locking in (b)
  now is a future wire-flip whose blast radius grows with each
  shipped consumer. Revisit when the second vendor backend is
  in flight and the multi-vendor case becomes concrete.

## Deferred to a future PR

- **`a{sv}` wire-format migration for vendor-config surfaces.**
  The architecturally cleaner end-state per Q6 — migrate
  `Manager1.AddTv` and `Manager1.UpdateTvConfig` from
  `config_json: s` to `config: a{sv}`. Triggers for
  reconsidering: (1) a second vendor backend lands and the
  multi-vendor introspection argument becomes concrete,
  (2) external tools (third-party clients, monitoring
  scripts) start consuming the D-Bus surface and the typed
  introspection becomes user-visible, (3) a vendor config
  needs nested structure that a{sv} would handle awkwardly
  but JSON would handle cleanly (which would *strengthen*
  the case for staying on JSON, not weaken it).
- **❓v1 Q5 — Validation timing.** Save-button-disabled-until-valid
  (matches the wizard's "Pair" button gating), or save-attempt-then-show-server-errors?
  My take: same as wizard — Save disabled while any required
  field is invalid; server-side errors surface inline on the
  originating field for cases the client validator can't catch.

## References

- [NetworkManager nm-settings(5)](https://manpages.debian.org/testing/network-manager/nm-settings.5.en.html)
  — UUID + name pattern; `nmcli connection modify <uuid|name>`.
- [GNOME Contacts Source-Edit
  pattern](https://gitlab.gnome.org/GNOME/gnome-contacts) —
  edit-pencil-toggle UX inspiration for the address-book-style
  shape user requested.
- plan5.md — provides the schema infrastructure
  (`config_schema`, `FieldVisibility`) this plan builds on.
