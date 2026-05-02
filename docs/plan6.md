# Plan 6 — Edit TV properties from the GUI

> **Status:** Draft. Builds on plan5's schema architecture
> (`config_schema` + `FieldVisibility`) which gave us the
> declarative metadata to drive an edit form for free. This plan
> adds the editing pathway end-to-end: daemon RPCs, atomicity,
> GUI shape, and the architectural cleanup that surfaced when
> we noticed `Tv1.Address` is a leaky vendor abstraction.

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

### ID vs name — research and recommendation ❓v1 Q1

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

For tisvlay, three viable shapes:

- **(a) Immutable ID.** ID is sanitized name at creation, never
  changes after. CLI accepts `name | id` for lookups (NetworkManager
  pattern). The user's "intuitive CLI" goal still holds — fresh
  pairings get the friendly ID; renames stop being CLI-visible.
  Pros: stable references everywhere (D-Bus paths, profile
  assignments, scripts, cron jobs). No collision-on-rename
  problem. Cons: ID drifts from name over time.
- **(b) Atomic rename-with-name.** ID = sanitized name; rename
  updates both. All references migrate atomically. Pros: most
  user-intuitive, ID always matches the visible name. Cons: have
  to coordinate atomic update across D-Bus paths, profile keys,
  in-flight subscriptions, GUI selection state, tray cache. Plus
  collision handling (rename "Office" → "Living Room" when a
  Living Room TV already exists).
- **(c) Hybrid: immutable ID, de-emphasized in UI.** Same as (a)
  but the GUI never surfaces the ID as a primary user-facing
  thing — it's a "machine identifier" in the INFO pane only.
  Most users won't notice the drift; power users see the
  stable handle.

**Recommendation: (a) with (c)'s UX polish.** Matches what every
mature daemon-style app does. Avoids the rename-coordination
mess entirely. The only loss is the rare power-user scenario
where they want to rename in the GUI and have their `cron` job
that runs `tisvlay tvs power-on office` keep working — for that,
NetworkManager's pattern of "CLI accepts name OR id" handles
both cases without the daemon-side complexity.

For collision: (a) means renames never produce a collision (only
new pairings can collide, and that's already handled by
`ConfigError::DuplicateTvId`).

Open as **❓v1 Q1**: confirm (a) + (c), or pursue (b)?

### `Tv1.Address` is leaky ❓v1 Q2

`Tv1.Address` exposes the LG-specific IP as a top-level cross-vendor
property. This was fine when LG was the only backend; now that the
schema-driven `Tv1.VendorInfo` exists (carrying the same IP under
its proper label), `Tv1.Address` is redundant — and worse, it's a
shape that future non-IP backends (USB-CEC at `/dev/ttyUSB0`,
Bluetooth at `XX:XX:XX:XX:XX:XX`) would have to shoehorn into.

The cleanup:

- Drop the `address` field from `TvService` and the `Tv1.Address`
  D-Bus property.
- Drop `TvInfo.address` from the GUI client.
- Display sites that currently read `address` switch to reading
  the relevant entry from `vendor_info` (already plumbed
  end-to-end in the prior commit).

`DiscoveredTv.address` is a parallel concern — pre-pair display
identifier returned from each vendor's discovery flow. It's
typed as a String not "IP" so the abstraction is technically
clean, but the field name is misleading. Two options:

- Keep `DiscoveredTv.address` (it's a UI string, not an IP).
- Rename to `display_id` or `display_string` to make the
  abstraction explicit.

Open as **❓v1 Q2**: drop `Tv1.Address` cleanly (no
backward-compat — the project has no live consumers); rename or
keep `DiscoveredTv.address` as-is?

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

### Existing methods that overlap

`Manager1.SetTvOption(tv_id, key, value)` already handles
top-level edits per-field. `UpdateTvConfig` is its batch +
vendor-aware sibling. We can either:

- Keep both (per-field for tray/CLI quick edits; batch for
  GUI's edit mode).
- Deprecate `SetTvOption` and route everything through
  `UpdateTvConfig`.

Open as **❓v1 Q3**: keep both, or migrate to `UpdateTvConfig`
exclusively?

## Side effects per field

| Field | Effect |
|---|---|
| `name` | `Tv1.Name` PropertiesChanged. ID stays (per Q1 rec). Tray menu refreshes. Trivial. |
| `system_profile` | Already wired via `SetTvOption`. Re-resolve any session bindings. |
| `primary_input` | Already wired. Affects `Tv1.Automated` recomputation. |
| `managed` | Already wired. Lifecycle automation respects new flag. |
| `ip` (vendor) | Connection drops. Daemon reconnects with new IP. NIC route re-resolves. Subscriptions re-bring-up (parallelized — fast). |
| `mac` (vendor) | No connection break. `Tv1.Power.SupportsWake` recomputes. Power-on button enables/disables. |

Only `ip` has nontrivial coordination. The flow already exists
inside `tv_manager::reconnect`; we just need to wire the
edit-applied → drop-and-reconnect path.

## Refactoring needs

1. **Hoist `SchemaForm` from `pairing_wizard.slint`.** The
   `for f in schema-fields: FieldRenderer { ... }` loop, the
   per-field validation lifecycle, the "Pair" button gating —
   all of it works for the edit pane unchanged. Extract to a
   shared component so wizard and edit pane both consume it.

2. **Hoist Rust validation helpers.** `autofill_schema`,
   `validate_value`, `compute_schema_valid`,
   `build_config_json_from_schema`,
   `parse_field_type` currently live in
   `tisvlay-gui/src/main.rs`. Move to a shared module
   (`tisvlay-gui/src/schema_form.rs`?) so the edit flow doesn't
   duplicate them.

3. **Drop `Tv1.Address`** (per Q2 above). Touches:
   - `TvService.address` cached field
   - `Tv1.Address` D-Bus property + proxy
   - `TvInfo.address` field
   - Every read site (tray, GUI header). Most should switch to
     `vendor_info` lookups; some (the wizard's discovered-list)
     stay on `DiscoveredTv.address`.

4. **`TvManager::update_tv_config(tv_id, json_patch)`** —
   new method coordinating the steps above. Implementation
   reuses existing primitives (`set_tv_option` for top-level,
   `wake_and_connect` for reconnect).

5. **`TvManager::get_tv_config_json(tv_id)`** — assembles the
   prefill payload by walking `config_schema` and reading
   current values from `tv.config`. Strips `Secret`.

## Phases

Suggested order, smallest reviewable units first:

- **Phase 8.0 — Drop `Tv1.Address`.** Pure refactor, no UX
  change. Everywhere that displayed `address` switches to
  reading `vendor_info` (or stays on `DiscoveredTv.address` for
  the pre-pair case). Frees the next phases from working
  around the leaky abstraction.
- **Phase 8.1 — Extract `SchemaForm` + Rust validation
  helpers.** Pure refactor. Wizard keeps working unchanged;
  shared infrastructure ready for edit pane.
- **Phase 8.2 — Daemon RPCs.**
  `Manager1.UpdateTvConfig(tv_id, json)` +
  `Manager1.GetTvConfig(tv_id)`. Server-side validation +
  atomicity + side-effect handling. Test via `busctl` before
  any GUI work.
- **Phase 8.3 — GUI edit-mode toggle.** Pencil icon in INFO
  group → row inputs + Cancel/Save. Reuses Phase 8.1 components.
  Wired to Phase 8.2 RPCs.
- **Phase 8.4 — Side-effect polish.** Reconnect-on-IP-change
  flow, `SupportsWake` refresh, status indicator surfaces a
  brief "applying…" / "connection lost" state during
  edit-with-side-effect.
- **Phase 8.5 (optional)** — CLI `tisvlay tvs <id|name> set
  <field>=<value>` shorthand wrapping `UpdateTvConfig`. Mirrors
  NetworkManager's `nmcli connection modify`.

## Open questions

- **❓v1 Q1 — ID strategy.** Confirm immutable ID + dual-lookup
  CLI (a + c), or pursue atomic rename-with-name (b)?
- **❓v1 Q2 — `Tv1.Address` cleanup.** Drop now (Phase 8.0), or
  keep as a deprecated shim while migrating? `DiscoveredTv.address`
  — keep, rename, or generalize?
- **❓v1 Q3 — `SetTvOption` future.** Keep both `SetTvOption` and
  `UpdateTvConfig`, or migrate to one path?
- **❓v1 Q4 — Edit-mode scope.** INFO group's pencil icon —
  edits only the INFO-group fields (TV ID excluded; name +
  vendor fields), or also pulls in fields currently inline
  elsewhere on the detail page (System profile, Primary input,
  Managed)? My take: keep inline controls where they are;
  edit mode covers fields that don't have a dedicated control
  yet (name + vendor fields). User's own answer in chat
  ("don't duplicate fields") aligns.
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
