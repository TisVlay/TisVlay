## Summary

Third attempt at landing profile management. v1 (PR #58, whole-body JSON CRUD) was rejected for per-edit GUI latency. v2 (PR #59, per-profile D-Bus objects with typed properties) shipped fast but over-corrected to +12 646 LoC of structural overhead. v3 resets to the property-bag wire shape v1 had (`a{sv}` bodies via `#[derive(SerializeDict, DeserializeDict)]`), keeps the UUID-as-stable-identity pattern v2 introduced (decouples display from identity for i18n + rename robustness), and fixes v1's latency with a client-side three-layer cache instead of re-fetching on every edit.

User-visible feature parity with v2: master/detail Profiles pane, create / rename / delete / duplicate, per-TV assignment, inheritance with locally-set indicator + per-field reset + reset-all, IdleTimeoutSeconds spinner, detail-header rename pencil, Used-by chip ticker with click-through, TVs ↔ Profiles visual parity, `Errors.profiles` inline banner, a11y, toasts, reserved-default materialization. The only architectural feature v2 had that v3 explicitly does not is per-field `PropertiesChanged` granularity — v3 substitutes per-body re-fetch on `SystemProfileChanged(uuid)`, which is one dict deserialize per edit, equivalent latency in practice.

## Wire surface

`org.tisvlay.Manager1` (system bus, daemon-owned) and `org.tisvlay.Session1` (session bus, session-owned) each grow eight CRUD methods plus three signals. Identity is UUID; the display name is a field of the body dict, so renames don't touch any reference site:

```
ListSystemProfiles() -> a{sa{sv}}                  ; uuid -> body
GetSystemProfile(uuid: s) -> a{sv}
UpdateSystemProfile(uuid: s, body: a{sv})          ; atomic full-body replace
CreateSystemProfile(name: s) -> s                  ; mints uuid; returns it
DuplicateSystemProfile(source_uuid: s, new_name: s) -> s
DeleteSystemProfile(uuid: s)                       ; rejects default uuid + referenced
GetResolvedSystemProfileDefaults() -> a{sv}        ; hard-coded baseline
GetEffectiveSystemProfile(tv_id: s) -> a{sv}       ; resolved-for-this-TV walk

signal SystemProfileChanged(uuid: s)               ; body mutation (incl. rename)
signal SystemProfileAdded(uuid: s)                 ; Create / Duplicate
signal SystemProfileRemoved(uuid: s)               ; Delete
```

Mirror set on `Session1` for user profiles. The pre-existing per-TV-assignment-change signal renames to `TvUserProfileChanged(tv_id, profile_uuid)` so the body-mutation signal can take the canonical `UserProfileChanged(uuid)` name — symmetric with Manager1's `SystemProfileChanged(uuid)`. The `Tv` prefix matches the existing `GetTvUserProfile` / `SetTvUserProfile` methods, and the naming convention becomes legible: methods + signals starting with `Tv` operate on a TV's assignment; without `Tv` they operate on the catalog.

Rename has no dedicated method — it's `UpdateSystemProfile(uuid, body_with_new_name)` with same atomic-replace semantics. `Added` fires once after persistence for both Create and Duplicate, under the same write lock as the body commit, so a client receiving `Added` can immediately `GetSystemProfile(uuid)` and get the body. `Update` is full-body atomic replace because the wire format (`Option<T>` + skip-if-`None`) only has two states per key and can't express "leave this field alone"; the GUI keeps a live cache of bodies, clones-patches-sends on edit.

## Body types

`tisvlay-core` exports two dict-derive structs feature-gated under `dbus`:

```rust
#[derive(Type, SerializeDict, DeserializeDict, Default, Clone, Debug)]
#[zvariant(signature = "a{sv}", deny_unknown_fields)]
pub struct SystemProfileBody {
    pub name:          String,                  // mandatory; mutable display label
    pub on_startup:    Option<SessionAction>,
    pub on_shutdown:   Option<SessionAction>,
    pub on_reboot:     Option<SessionAction>,
    pub on_no_session: Option<SessionAction>,
    pub on_sleep:      Option<SessionAction>,
    pub on_wake:       Option<SessionAction>,
    pub automate_when: Option<AutomateWhen>,
}

#[derive(Type, SerializeDict, DeserializeDict, Default, Clone, Debug)]
#[zvariant(signature = "a{sv}", deny_unknown_fields)]
pub struct UserProfileBody {
    pub name:                 String,
    pub on_session_lock:      Option<SessionAction>,
    pub on_session_unlock:    Option<SessionAction>,
    pub on_greeter_wake:      Option<SessionAction>,
    pub on_idle_timeout:      Option<SessionAction>,
    pub on_idle_resume:       Option<SessionAction>,
    pub idle_timeout_seconds: Option<u64>,
}
```

`name` is mandatory because every profile has one; the other fields are inheritable (`None` ⇒ key absent ⇒ inherits from the chain below). `deny_unknown_fields` rejects misspelled field names at the dict-derive boundary instead of silently absorbing them as `None`. `SessionAction` + `AutomateWhen` grow `Type` derive + `#[zvariant(signature = "s")]` with a `// SAFETY:` invariant comment locking them as unit-variant-only.

The reserved-default UUIDs (`DEFAULT_SYSTEM_PROFILE_UUID` / `DEFAULT_USER_PROFILE_UUID`) anchor the inheritance ancestor every other profile falls back to. Their `name` field is locked to the literal `"default"` so the daemon's inheritance walker has a stable wire-recognizable ancestor and the GUI's `(default)` badge has a stable predicate. The GUI renders the displayed label through `gettext("default")` — wire identity is the UUID, the rendered label is a client concern.

## Referenced-by computation (client-side, not a typed property)

v2 had `ReferencedBy: as` as a typed property on each per-profile D-Bus object. v3 drops the typed property; the computation moves client-side via a shared helper:

```rust
pub fn references_of(profile_uuid: Uuid, tvs: &[TvAssignment]) -> Vec<Uuid>;
pub fn user_references_of(profile_uuid: Uuid, tvs: &[TvAssignment]) -> Vec<Uuid>;
```

Contract: a TV references `profile_uuid` iff (a) its `system_profile` field equals `profile_uuid`, OR (b) its `system_profile` is unassigned AND `profile_uuid == DEFAULT_SYSTEM_PROFILE_UUID`. The fan-out branch (b) makes the helper non-trivial — every unassigned TV implicitly references the reserved-default profile, so deleting the default would orphan every such TV. Daemon and clients both call the same helper, single-sourcing the fan-out rule. Delete is server-authoritative; client-side disable is advisory.

## GUI editor

`tisvlay-gui/src/profiles.rs` is the editor module. `ProfileEditorState` owns one three-layer `ProfileLayers` cache per kind (system + user):

```rust
struct ProfileLayers {
    baseline: SystemProfileBody,                // static — fetched once at GUI start
    default_body: SystemProfileBody,            // re-fetched on SystemProfileChanged(DEFAULT_*_UUID)
    bodies: HashMap<Uuid, SystemProfileBody>,   // re-fetched on SystemProfileChanged(uuid)
}
```

Three layers, fetched on signal, not on every edit — v1's perf failure was re-fetching all three per edit. The resolution walker `resolve_field(layers, uuid, field) -> ResolvedValue` returns either `LocallySet(value, current_string)` or `Inheriting(resolved_value, resolved_string)` with a trailing `*` overlay on the `current_string` (enum rows only — number rows lean on gutter + reset glyph alone since SpinBox doesn't have the ComboBox no-op-on-same-value problem). Field-kind registry (`FieldSpec`, `FieldKind::Enum / Number / Bool / Text`) lives in this module, not in `tisvlay-core` — server / session sides don't need a field registry; they just deserialize the body dict.

Edits commit atomically: clone the cached body, patch one field, `Manager1.UpdateSystemProfile(uuid, body)`, daemon validates + persists + fires `SystemProfileChanged(uuid)`, GUI's signal handler re-fetches the body and refreshes the field rows. One D-Bus round-trip per edit. Per-kind `AtomicU64` generation counters protect against stale callbacks during reselection (replaces v2's per-profile-proxy generation-counter pattern).

## Notable architectural decisions

- **Atomic full-body replace, not partial patch.** The wire format only has two states per key (present / absent), so it can't express "leave this field alone." Matches `NetworkManager.Settings.Connection.Update` shape. Concurrent body updates are last-write-wins; the realistic loss scenario is scripted CLI loops interleaved with an open GUI session. ETag-style optimistic concurrency is deferred to a future `UpdateSystemProfileChecked(uuid, body, expected_version: t)` method when multi-client editing becomes a real workflow.

- **No cross-bus signal re-fire.** The user-profile inheritance chain crosses bus boundaries (`user → user-default → admin baseline → admin user-default → hardcoded baseline`). The session daemon does *not* re-fire `Session1.UserProfileChanged` when an admin-baseline body mutates — clients subscribe to both buses directly. The GUI already does this; saves ~120 LoC of session-as-signal-router complexity for no current consumer.

- **`Session1` signal naming.** Two semantically distinct events share the user-profile noun: a per-TV assignment change (which UUID a TV references) vs a body mutation (the body content of a UUID). To keep names symmetric with `Manager1.SystemProfileChanged(uuid)`, the body-mutation signal takes the canonical `UserProfileChanged(uuid)` name and the assignment-change signal renames to `TvUserProfileChanged(tv_id, profile_uuid)`. The `Tv` prefix tracks the existing `GetTvUserProfile` / `SetTvUserProfile` method shape; reader convention becomes "anything starting with `Tv` operates on a TV's assignment, without `Tv` operates on the catalog". Manager1 didn't face this collision because the system-bus per-TV assignment writes through the standard `Tv1.SystemProfile` property surface, not a custom signal.

- **`ReloadConfig` emits per-profile signals.** The plan didn't anticipate this; surfaced during smoke testing. A config-file edit + Reload click had the master list refreshing (via the coarse `ConfigReloaded` re-fetch) but the editor's `ProfileLayers.bodies` cache stayed stale until the user clicked away and back. Fixed: `ReloadDiff` grows `system_profiles_{added,removed,changed}: Vec<String>`; `tv_manager::reload_config` snapshots the old section under the write lock via `std::mem::take`, computes a three-way diff; `apply_reload_diff` emits the matching `Manager1.SystemProfile{Added,Changed,Removed}` signals. `Session1.reload_config` mirrors this for `UserProfile{Added,Changed,Removed}`. The architectural takeaway: a config-file edit should be observationally identical to an external client mutation — same signals, same client cache invalidation.

## Commits

Twenty-two commits in roughly five phases (Phase 5 docs migration into `docs/design.md` + plan deletion is deferred to a follow-on PR):

**Phase 1 — Plan (6 commits, plan absorbed two rounds of agent review + a progress log at the tail):**

- `ecdda98` docs(plans): profile-CRUD v3 — property-bag wire surface
- `431f471` docs(plans): profile-CRUD v3 — property-bag wire over UUID identity
- `cbb801a` docs(plans): profile-CRUD v3 — incorporate agent-review findings
- `c10d3e2` docs(plans): profile-CRUD v3 — apply four-agent review findings
- `54c876e` docs(plans): profile-CRUD v3 — drop body_version + cross-bus re-fire
- `d653779` docs(plans): profile-CRUD v3 — progress log + carry-partial + reload-bug lessons

**Phase 2 — UI polish (4 commits, wire-independent):**

- `eeaf988` feat(gui): widgets + icons for Profiles pane
- `ad32f9c` feat(gui): design tokens for Profiles ↔ TVs parity
- `0dd3e51` feat(gui): Profiles-pane Slint shell — master list + detail header
- `919c7d9` feat(gui): Errors.profiles slot + dual-route reload banner

**Phase 3 — Daemon + session surface (5 commits; commit 6 split into 4 sub-commits):**

- `2f4b310` feat(core): SystemProfileBody + UserProfileBody dict-derive types
- `c1f5d8a` feat(core,daemon,session,cli): TV profile references migrate to UUIDs
- `39069b7` feat(daemon,core): Manager1 v3 CRUD surface + tv_manager handlers
- `41798a1` feat(core): DaemonClient system-profile CRUD wrappers + 13 integration tests
- `d8e29e7` feat(session,core): Session1 user-profile CRUD mirror + 13 integration tests

**Phase 4 — GUI rebind (3 commits):**

- `10b3aa5` feat(gui,core): state.rs + state_watch.rs rebind to property-bag CRUD surface
- `d9f4d6a` feat(gui): profiles.rs editor rebuild — ProfileLayers cache + signal-driven row refresh
- `f58addf` feat(gui): profiles dispatch helpers + Slint callback wiring

**Carry-partial follow-ups (2 commits, surfaced via smoke testing after Phase 4):**

- `e46200b` fix(gui): TVs-pane profile comboboxes display the name, not the UUID
- `311f4d5` refactor(gui): route TVs-pane pencil → Profiles navigation by UUID

**Bug fix (1 commit, not anticipated by the plan):**

- `4dac4c6` fix(daemon,session): emit per-profile signals on ReloadConfig

**Naming cleanup (1 commit):**

- `refactor(session,core): rename Session1 body-mutation signal to UserProfileChanged` — pure rename for naming symmetry between Manager1 + Session1 (see "`Session1` signal naming" decision above). The pre-existing assignment signal becomes `TvUserProfileChanged(tv_id, profile_uuid)`; the body-mutation signal takes the canonical `UserProfileChanged(uuid)` name.

## Footprint

~+12 000 / −570 LoC across ~75 files vs `origin/main`. Test count: existing 380-something baseline + ~120 new tests (13 daemon CRUD + 13 session CRUD + 12 editor unit tests + 21 dispatch unit tests + plus per-test integration coverage across the carry-partial fixes + the reload-signals fix) = ~500 passing on the branch. Per-test floor stayed ~98 LoC of harness boilerplate. Architectural savings against v2: ~3 600 LoC by dropping per-profile D-Bus object infrastructure + state-forwarder cross-profile fan-out + per-property typed properties.

## Test plan

- [ ] `just check` end-to-end green on the merge commit.
- [ ] `prek run -a` end-to-end green on the merge commit.
- [ ] Manual GUI smoke test:
  - [ ] Profile create flow (System + User).
  - [ ] Profile rename flow (default + non-default).
  - [ ] Profile duplicate flow.
  - [ ] Profile delete flow (default rejected, referenced rejected, unreferenced succeeds).
  - [ ] Per-field edit + reset + reset-all on both kinds.
  - [ ] IdleTimeoutSeconds spinner edit + reset.
  - [ ] Cross-client live refresh (edit via a scripted D-Bus client, watch GUI update without manual reload).
  - [ ] Config-file edit + Reload click → editor pane reflects the new field values immediately (regression check for the `ReloadConfig` signal fix).
  - [ ] TVs-pane PROFILES-row ComboBox displays profile names, not UUIDs (regression check for the carry-partial fix).
  - [ ] TVs-pane PROFILES-row pencil opens the Profiles pane with the assigned profile pre-selected.
  - [ ] Reload-error inline banner surfaces visibly when the daemon's reload fails.
- [ ] Per-edit perceived latency ≤ 50 ms (the v2 bar carries forward).

## Follow-ups not in this PR

- **Phase 5 docs migration** — `docs/design.md` grows the v3 `Manager1` / `Session1` surface + body-dict reference + inheritance subsection per the plan's commit 11 sketch. `docs/plans/profile-crud-v3.md` deletes; git history is the archive. Architectural lessons worth preserving into `docs/design.md`:
  - Configuration over D-Bus is property-bag-shaped, not typed-property-per-object — the BlueZ pattern works for operational state, not for configuration.
  - A config-file edit should be observationally identical to an external client mutation (same signals, same client cache invalidation).
  - For multi-stage refactors with a v2-style triage table: the wire-rebind commit's checklist must explicitly enumerate every `CARRY-PARTIAL` row's required rework, not just the obvious wire-coupled portion.
- **ETag-style optimistic concurrency** — when multi-client editing becomes a real workflow, ship `UpdateSystemProfileChecked(uuid, body, expected_version: t)` as a new method with a typed `org.tisvlay.Error.Conflict` rejection. Genuinely additive on the wire.
- **i18n catalog refresh** — `just i18n-extract` + `just i18n-update` against the new user-facing strings (dialog copy, helper text, error toasts, FIELDS labels). Lands with the Phase 5 docs commit.
