# Profile-CRUD v3: property-bag wire surface

## Motivation

This is the third attempt at landing profile management. The first two are instructive context, not noise — every architectural choice in v3 traces back to a lesson from v1 or v2.

**v1** (PR #58, `feat/profile-crud`, 13 commits, +4 947 / −131 LoC, 28 files): whole-body JSON CRUD on `Manager1` / `Session1`. `Get`/`Create`/`Update`/`Delete` carrying a JSON-serialized `SystemProfileConfig` plus `GetResolvedProfileDefaults` for the inheritance baseline. The GUI computed three-layer inheritance client-side (`profiles_state.rs`) and re-fetched all three layers on every field edit + rebuilt the Slint model. **Rejected** during smoke testing because the per-edit latency was visible to the user — every ComboBox click produced a JSON parse, an `UpdateProfile` round-trip, three further re-fetches, and a full model rebuild.

**v2** (PR #59, `feat/profile-crud-v2`, 44 commits, +12 646 / −2 077 LoC, 86 files): lifted profiles to first-class D-Bus objects under `/org/tisvlay/{System,User}Profile/<UUID_>` with one typed `#[zbus(property)]` per field, mirroring the per-TV `Tv1.*` pattern. Field edits become single `Properties.Set` round-trips. `LocallySetFields: as` companion property carries the inheritance status; `ReferencedBy: as` lists referencing TV UUIDs; per-profile object adds `ResetField` + `ResetAll` methods. UUID-keyed both on disk and on the wire so rename doesn't have to walk and rewrite references.

v2 fixed the responsiveness problem but at a structural cost that grew through the implementation:

- 3 000 LoC of per-profile D-Bus object infrastructure (server-side interface impls + register / unregister + the `state_forwarder`'s cross-profile fan-out machinery).
- 3 000 LoC of integration tests across ~40 files exercising the per-property surface.
- 800 LoC of GUI per-profile proxy + listener + generation-counter machinery to consume the per-property `PropertiesChanged` granularity.
- 500 LoC of UUID migration touching every consumer (CLI, tray, GUI, configs).
- 600 LoC of `tisvlay-core` follow-up (`ProfileManager` trait + `profile_fields` registry) trying to consolidate the duplication that fell out of the structural choice.

After two rounds of cleanup the net delta against `origin/main` is +133 code lines, but only because most of the v2 commits added structural overhead the cleanup rounds then trimmed. The "more concise idiomatic code" goal failed at the architectural level.

**v1 was right that whole-body CRUD is the idiomatic shape** for configuration over D-Bus — `NetworkManager.Settings.Connection.Update(a{sa{sv}})`, `systemd-homed.Home.Update(s)`, `power-profiles-daemon`'s `Profiles: aa{sv}` all use property-bag bodies, not per-field typed properties. **v1 was wrong about how to make the GUI fast** — the three-layer re-fetch was unnecessary work, not a wire-protocol shortcoming. The hardcoded baseline never changes; the "default" profile body changes rarely; only the selected body ever needs re-fetching on an edit, and even then it's one round-trip, not three.

**v2 corrected the wrong thing** — it kept the typed wire surface as the GUI's data source (good) but rewrote the wire shape from configuration-idiomatic (property bag) to operational-state-idiomatic (typed-property-per-object). The result was fast but structurally over-engineered for what is fundamentally configuration data: 5–7 mostly-static fields per profile, 1–10 profiles per catalog.

**Wire ergonomics**: zbus's dict-derive macros (`SerializeDict` / `DeserializeDict`) make the property-bag pattern ergonomic in Rust — both ends operate on typed structs while the wire stays `a{sv}`. Verified by a small round-trip fixture against the workspace's current pin (`zbus` 5.x) that the proposed body shape works as designed. An earlier draft of this plan claimed `zbus` 5.11+ was required for `Option<u32>` support; that claim was wrong — 5.10 handles the shape fine. The dep stays at the workspace's current 5.x pin.

This plan keeps the property-bag wire shape v1 had, keeps the UUID-as-stable-identity pattern v2 introduced (decouples display from identity for i18n + rename robustness across client caches), fixes v1's GUI-side latency with a layer cache, and carries forward every UI polish + dialog primitive + widget + token + bug fix v2 contributed.

## v3 architecture

### Wire surface

Two interfaces grow CRUD methods + change signals. No per-profile D-Bus objects. No `ObjectManager` walking. No per-property `PropertiesChanged` granularity. Identity is UUID; the display name is a field of the body dict.

**`org.tisvlay.Manager1`** (system bus, daemon-owned):

```
ListSystemProfiles() -> a{sa{sv}}                                ; uuid → body
GetSystemProfile(uuid: s) -> a{sv}                               ; one body
UpdateSystemProfile(uuid: s, body: a{sv})                        ; full-body atomic replace
CreateSystemProfile(name: s) -> s                                ; mint uuid; returns it
DuplicateSystemProfile(source_uuid: s, new_name: s) -> s         ; mint uuid for clone
DeleteSystemProfile(uuid: s)                                     ; rejects default uuid + referenced
GetResolvedSystemProfileDefaults() -> a{sv}                      ; static baseline
GetEffectiveSystemProfile(tv_id: s) -> a{sv}                     ; resolved-for-this-TV walk

signal SystemProfileChanged(uuid: s)                             ; body mutation (incl. rename)
signal SystemProfileAdded(uuid: s)                               ; Create / Duplicate
signal SystemProfileRemoved(uuid: s)                             ; Delete
```

`Update` is **atomic full-body replace**, not partial patch. The wire format (`Option<T>` + skip-if-`None`) only has two states per key — present or absent — so it can't express "leave this field alone." The GUI keeps a live cache of bodies (re-fetched on `SystemProfileChanged`); commit-on-edit clones the cached body, patches one field, and sends the whole thing back. Per-edit payload is ~14 keys (sub-kilobyte). The daemon's `From<SystemProfileBody> for SystemProfileConfig` overwrites the on-disk body wholesale. Matches the `NetworkManager.Settings.Connection.Update` shape v3 cites as prior art.

Rename has no dedicated method — it's `UpdateSystemProfile(uuid, body_with_new_name)` with the new name in the body. Same atomic-replace semantics. The daemon validates the new name (non-empty, unique, not the reserved `"default"` literal when applied to non-default UUIDs) and fires `SystemProfileChanged(uuid)`. TV references stay valid because `Tv1.SystemProfile` carries the UUID. Stale-name races during rename are eliminated by construction — clients cache by UUID, the only mutable thing is the display label.

Catalog topology changes (Create / Duplicate / Delete) emit `Added(uuid)` and `Removed(uuid)` rather than a single coarse list-changed signal so clients don't have to re-fetch the full list — the signal carries which UUID moved. `SystemProfileChanged(uuid)` stays separate because body mutation is conceptually distinct from catalog churn.

**`Added` signal contract**: fires once, after persistence, for both Create (empty body) and Duplicate (copy of source body). Clients fetch the body on receipt — no separate `Changed` will follow for the just-added profile. Both flows commit body + emit signal under the same write lock so the body is fetchable the moment the client receives `Added`.

**`org.tisvlay.Session1`** (session bus, session-owned): mirror surface for user profiles + the existing `SetTvUserProfile` / `GetTvUserProfile` per-TV assignment methods (unchanged from main). Both take UUIDs.

### Body types

`tisvlay-core` exports two `#[derive(Type, SerializeDict, DeserializeDict, Default, Clone, Debug)]` structs:

```rust
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

#[zvariant(signature = "a{sv}", deny_unknown_fields)]
pub struct UserProfileBody {
    pub name:                 String,
    pub on_session_lock:      Option<SessionAction>,
    pub on_session_unlock:    Option<SessionAction>,
    pub on_greeter_wake:      Option<SessionAction>,
    pub on_idle_timeout:      Option<SessionAction>,
    pub on_idle_resume:       Option<SessionAction>,
    pub idle_timeout_seconds: Option<u32>,
}
```

`name` is mandatory because every profile has one. The other fields are inheritable: `None` ⇒ key absent ⇒ inherits from the chain below.

The dict-derive macros serialize `Option<T>::None` fields as **absent keys** in the dict, not as null entries. `Some(value)` → key present with the variant-wrapped value. This is exactly the inherit-vs-locally-set distinction v2 needed `LocallySetFields` to express: present ⇒ locally set, absent ⇒ inheriting. **`LocallySetFields` is no longer a separate wire concept.**

`deny_unknown_fields` is opt-in on the dict-derive (verified against `dict.rs:165-169` in the macro crate — without it, unknown keys are silently dropped, defeating the present-vs-absent semantics). With it, a misspelled field name from a client errors out at the boundary instead of silently absorbing as `None`.

The `SessionAction` and `AutomateWhen` enums need the dict-derive's `Type` companion plus an explicit string-signature attribute (`signature = "s"`) on the type definition (in `tisvlay-core::config`) so the dict-derive macro can compute the variant signature for the body's enum-typed fields. Currently they only have `Serialize` / `Deserialize` / `Default` — add the missing derives in Commit 6's checklist.

**Invariant comment required on both enums**: the string-signature attribute is a hand-declared promise that the enum stays unit-variant-only — adding a struct variant later silently diverges wire signature from actual emit with no compile error. A `// SAFETY:` style comment locks the contract.

The reserved-default UUIDs (`DEFAULT_SYSTEM_PROFILE_UUID` / `DEFAULT_USER_PROFILE_UUID`) keep their v2 semantics: their `name` field is locked to the literal `"default"` so the daemon's inheritance walker has a stable wire-recognizable ancestor and the GUI's `(default)` badge has a stable predicate. The GUI renders the displayed label through `gettext("default")` — wire identity is the UUID, the rendered label is a client concern.

### Referenced-by computation

v2 had `ReferencedBy: as` as a typed property on each per-profile D-Bus object — daemon computed which TVs referenced a given profile, including the inheritance fan-out rule (a TV with no explicit profile assignment implicitly references the default). v3 drops the typed property; the computation moves client-side.

**One source of truth.** A helper `tisvlay_core::references_of(profile_uuid, tvs: &[TvAssignment]) -> Vec<Uuid>` encodes the fan-out rule once. Daemon and clients both call it. The helper's contract: a TV references `profile_uuid` iff (a) the TV's `system_profile` field equals `profile_uuid`, OR (b) the TV's `system_profile` is empty / unassigned AND `profile_uuid == DEFAULT_SYSTEM_PROFILE_UUID`. Mirror helper `user_references_of` for the user side.

**Client consumers.** Master-list status (Active / Assigned / Unassigned), detail header "Used by [chip][chip]" ticker, delete-confirm dialog's "still used by X, Y" wording, delete-disable predicate. All call `references_of` against the in-memory TV list the GUI already maintains. No new wire round-trips, no new daemon method.

**Delete is server-authoritative; client-side disable is advisory.** A user assigning a TV via CLI while the GUI's delete-confirm dialog is open will compute a stale reference set on the client. The daemon's `DeleteSystemProfile` re-checks under its own lock and rejects on outstanding references — the worst case is a user-visible "still referenced" error after clicking a button the GUI thought was safe. That's acceptable; parity between client-cached and daemon-authoritative reference sets is not achievable without a per-call round-trip, which the design explicitly rejects.

**Why not a daemon method.** The TV list is already pushed to every client; computing references from it is cheap (linear scan in profile count × TV count). A `Manager1.GetSystemProfileReferences(uuid)` round-trip on every master-list refresh + delete-confirm open would add a per-operation cost for state every client already has.

**Why not in the body dict.** Mixing persisted state (the profile body's configuration fields) with computed state (live references) breaks the deserialize → mutate → re-serialize round trip — the computed field would go stale the moment the cached body is re-sent.

### Cross-profile inheritance signaling

When the default profile's body changes, every *inheriting* profile's resolved values shift. The daemon fires `SystemProfileChanged(DEFAULT_SYSTEM_PROFILE_UUID)` once. Clients caching layers (`baseline` + `default_body` + `bodies[uuid]`) re-resolve on demand — no per-profile cascade is needed because the cache structure separates the layers. Clients that cache *resolved* values (rather than layers) must subscribe to `SystemProfileChanged` on the default UUID and recompute.

**User-side cross-bus gap.** The user-profile inheritance chain crosses bus boundaries: `user → user-default → admin baseline named profile → admin user-default → hardcoded baseline`. The last two layers live in the daemon's system-config TOML (administrator-provisioned baselines for user profiles, surfaced through `Manager1`), not the session's user-config. When the daemon mutates one of those system-side admin-baseline layers, the *session bus* doesn't fire `UserProfileChanged` automatically — but every session-side user profile's resolved values shift.

Resolution: the session daemon subscribes to `Manager1.SystemProfileChanged(uuid)` for the admin-baseline UUIDs and re-fires `Session1.UserProfileChanged(uuid)` for every session-side user profile that inherits from them. Cheap (the admin-baseline catalog is small + rarely mutated). Lands in commit 7.

### Inheritance model

Resolution stays server-side, identical to v2:

- System: `this → default profile → hardcoded baseline (ResolvedSystemProfile::default())`
- User: `this → user default → admin baseline named profile → admin user default → hardcoded baseline`

The walker code in `SystemSection::resolve_profile_by_uuid` / `UserSection::resolve_profile_by_uuid` keeps working as-is once we re-key from UUID to name. `GetEffectiveSystemProfile(tv_id)` returns the fully-resolved body for one TV (the GUI does not need this directly — it has the layers cached — but the CLI uses it for `tisvlay profile show <tv>`).

The GUI cache:

```rust
struct ProfileLayers {
    baseline: SystemProfileBody,                // static — fetched once at GUI start
    default_body: SystemProfileBody,            // re-fetched on SystemProfileChanged(DEFAULT_SYSTEM_PROFILE_UUID)
    bodies: HashMap<Uuid, SystemProfileBody>,   // re-fetched on SystemProfileChanged(uuid)
}
```

Three layers, fetched on signal, not on every edit. v1's perf failure was re-fetching all three per edit — that's gone.

### Reference shape

TVs reference profiles by UUID, same as v2: `[[tv]] profile.system = "<uuid>"`, `[[tv]] profile.user = "<uuid>"`. `Tv1.SystemProfile` property is the UUID string. The GUI resolves UUID → display name via the cached profile list.

### Why UUIDs (decoupled from display name)

Two independent reasons:

1. **i18n decoupling.** Profile names are user-facing strings. If names were wire identifiers, the reserved `"default"` literal couldn't be localized — `gettext("default")` would break the daemon's name-recognition for the inheritance ancestor. UUIDs decouple the two: wire identity stays `DEFAULT_SYSTEM_PROFILE_UUID`, the rendered label is a client-side gettext lookup.

2. **Stable refs across rename.** A tray menu rendered with `"Office"` is clicked half a second after another client renamed the profile to `"Living Room"`. Names-as-refs would have the daemon return "profile not found." UUIDs make the click still resolve; the menu just re-renders with the new label on the next signal. The same shape protects any other cached cross-reference (TV-assignment displays, error toasts referencing a profile that was just renamed, scripted CLI calls binding to a display label).

### Rename mechanics

Rename has no dedicated D-Bus method — it's `UpdateSystemProfile(uuid, body_with_new_name)`. The daemon under one write lock:

1. Validates the new name (non-empty, unique within catalog, not the reserved `"default"` literal when applied to a non-default UUID).
2. Updates the profile body's `name` field in place under the existing UUID key.
3. Persists.
4. Drops lock, emits `SystemProfileChanged(uuid)`.

TV references don't move because the UUID didn't move. Clients refresh the displayed label on the signal.

### Client API

`tisvlay-core::dbus_client` grows typed wrappers:

```rust
impl DaemonClient {
    pub async fn list_system_profiles(&self) -> Result<HashMap<Uuid, SystemProfileBody>>;
    pub async fn get_system_profile(&self, uuid: Uuid) -> Result<SystemProfileBody>;
    pub async fn update_system_profile(&self, uuid: Uuid, body: &SystemProfileBody) -> Result<()>;
    pub async fn create_system_profile(&self, name: &str) -> Result<Uuid>;
    pub async fn duplicate_system_profile(&self, source: Uuid, new_name: &str) -> Result<Uuid>;
    pub async fn delete_system_profile(&self, uuid: Uuid) -> Result<()>;
    pub async fn get_resolved_system_profile_defaults(&self) -> Result<SystemProfileBody>;
    pub async fn get_effective_system_profile(&self, tv_id: &str) -> Result<SystemProfileBody>;
}
```

Mirror set on `SessionClient` for user profiles. `#[derive(DeserializeDict)]` does the marshalling; no `HashMap<String, OwnedValue>` boilerplate.

### Editor wire shape

The GUI's editor commits an edit by:

1. Cloning the cached body for the selected profile.
2. Patching the one field (`body.on_startup = Some(SessionAction::ScreenOff)` to set, `= None` to reset).
3. One `Manager1.UpdateSystemProfile(uuid, body)` round-trip.
4. Daemon validates + persists + fires `SystemProfileChanged(uuid)`.
5. GUI's signal handler re-fetches the body and refreshes the field rows.

One D-Bus round-trip per edit, exactly matching v2's per-property latency. The body re-fetch is one trivial dict deserialize — much cheaper than v2's per-property `Properties.Get` per row.

### Inheritance display in the editor

For each field row:

- **Locally set**: `body.<field>.is_some()`. Left gutter accent renders; reset glyph visible. Displayed value is the locally-set value.
- **Inheriting**: `body.<field>.is_none()`. No gutter accent; no reset glyph. Displayed value is computed by walking `default_body.<field>.or(baseline.<field>)` (system) or the longer user chain.

The `<resolved>*` inherit-placeholder convention from v2 carries forward (commit 33 of v2).

### Field-kind dispatch

Carry from v2 (commit 42):

- `FieldKind::Enum { options }` and `FieldKind::Number { min, max, step, unit }` const tables.
- ProfileFieldEntry Slint struct's `kind` discriminator.
- `ProfileFieldRowEnum` + `ProfileFieldRowNumber` Slint components.
- `Bool` and `Text` placeholder variants in `FieldKind`.

**Where the tables live**: in `tisvlay-gui/src/profiles.rs` directly. v2 moved them into `tisvlay-core::profile_fields` to dedup between server + GUI + session, but the server / session sides don't need a field registry — they just deserialize the body dict into `SystemProfileBody` / `UserProfileBody` and that's the schema. Only the GUI needs the registry (for the row dispatch + the user-facing labels), so it lives there. Saves the cross-crate import + the `tisvlay-core::profile_fields` module.

## What carries from v2

Forty-four commits to triage. Each is one of:

- **CARRY** — cherry-pickable as-is (or with trivial conflict resolution against the missing per-profile-object infrastructure).
- **RE-IMPLEMENT** — the user-visible feature lands in v3 but via a different commit because the underlying machinery is different.
- **DROP** — purely architectural, no v3 analog.
- **DOC-ONLY** — plan doc or design doc; will be re-written or absorbed into the v3 plan / design.md migration.

| # | sha | One-line | Decision | v3 home |
|---|---|---|---|---|
| 1 | `0662377` | docs(plan): v2 plan | DOC-ONLY | superseded by this doc |
| 2 | `f45ae07` | profile UUIDs + per-profile D-Bus object scaffolding | RE-IMPLEMENT | carry UUID-keyed catalog + `name` field + reserved-default UUID constants; drop per-profile D-Bus registration |
| 3 | `4701e2b` | typed properties on SystemProfile1 + cross-profile fan-out | DROP | property-bag instead |
| 4 | `35255de` | typed properties on UserProfile1 + cross-profile fan-out | DROP | property-bag instead |
| 5 | `77cfd56` | TV profile refs migrate to UUIDs | CARRY-PARTIAL | UUID type changes + TOML format + `Tv1.SystemProfile` validator carry; `list_*_profiles_with_ids` ObjectManager walkers in `dbus_client.rs` rewrite against `Manager1.ListSystemProfiles` |
| 6 | `d3a206d` | Manager1 + Session1 Create/Delete + drop list RPCs | RE-IMPLEMENT | broader Manager1 surface (Update + Duplicate; rename folds into Update) |
| 7 | `b356834` | rebind Profiles pane master list to ObjectManager | RE-IMPLEMENT | bind to `Manager1.ListSystemProfiles` + `SystemProfileAdded` / `Removed` |
| 8 | `774750f` | bind ProfileFieldRow to per-profile D-Bus properties | RE-IMPLEMENT | bind to cached body + `SystemProfileChanged` |
| 9 | `6afd4df` | rename UX on profile detail header | RE-IMPLEMENT | wire to `UpdateSystemProfile` with a name-only body patch |
| 10 | `000139d` | show profile detail on same frame as click | CARRY | Slint global `in-out` fix; orthogonal |
| 11 | `56c06ee` | Errors.profiles banner + a11y | CARRY | Errors slot + a11y; signal-routing detail re-implemented |
| 12 | `f3a1d8a` | register UserProfile1 D-Bus objects after late daemon connect | DROP | no per-profile objects to register |
| 13 | `8b45e1a` | clear "Session service unavailable" on SessionAppeared | CARRY | session-available sync; orthogonal |
| 14 | `53fc607` | TVs-pane combobox display name not UUID | CARRY-PARTIAL | Slint + `convert_snapshot` UUID→name resolve carries; the `list_system_profiles_full` call site in `lib.rs` rewires to `Manager1.ListSystemProfiles` |
| 15 | `cbf9ff3` | route TVs-pane pencil → Profiles by UUID | CARRY-PARTIAL | UUID-keyed navigation carries; the `Profiles.system` / `Profiles.user` source rewires to the new list method |
| 16 | `f1c71d3` | tighten Profiles-pane ComboBox rows | CARRY | row-padding fix; orthogonal |
| 17 | `383349e` | strip tombstone comments | DROP | (was reverted in v2) |
| 18 | `60f7f59` | revert tombstone strip | DROP | (revert of #17) |
| 19 | `835d6c8` | profile duplication | RE-IMPLEMENT | `Manager1.DuplicateSystemProfile(source, new)` daemon-side clone |
| 20 | `505ca7e` | hoist "Reset all" onto FIELDS group title row | CARRY | layout-only; orthogonal |
| 21 | `13b65fa` | top-align TVs + Profiles pane detail bodies | CARRY | layout-only; orthogonal |
| 22 | `082e413` | split Profiles master list 50/50 | CARRY | layout-only; orthogonal |
| 23 | `e3840b1` | Icons.copy + Icons.delete | CARRY | icon-pool addition |
| 24 | `ae30613` | docs(plan): TVs↔Profiles parity | DOC-ONLY | absorbed into this plan |
| 25 | `f36309e` | docs(plan): redesign master row + header | DOC-ONLY | absorbed |
| 26 | `94dded2` | ProfileTile widget + Icons.im_user / system_users | CARRY | widget addition |
| 27 | `91d8b46` | derive active state per ProfileRow | RE-IMPLEMENT | active = derived client-side via `references_of(uuid, tvs).any(automated)` (see "Referenced-by computation" above) |
| 28 | `55744f5` | redesign master-list row in TVs shape | CARRY | layout shape; orthogonal |
| 29 | `0034ca9` | detail header 3-column shape | CARRY | layout shape; orthogonal |
| 30 | `7420668` | align detail-header height with TVs | CARRY | Flickable + plain-text spans; orthogonal |
| 31 | `a9fd81c` | tokenize header/body padding + LinkText | CARRY | tokens + widget; orthogonal |
| 32 | `7cceb52` | increase ProfileListRow height | CARRY | layout; orthogonal |
| 33 | `f4f9350` | inherit-placeholder is `<resolved>*` | CARRY | display convention; orthogonal |
| 34 | `22f09f6` | drop inherit-placeholder from ComboBox dropdown | CARRY | ComboBox shape; orthogonal |
| 35 | `a1c9fc1` | docs: migrate v2 + parity plans into design.md | DOC-ONLY | superseded; v3's design.md migration will be similar in shape |
| 36 | `590bfde` | materialize reserved-default profiles when TOML omits the section | CARRY | same `ensure_default` invariant on the UUID-keyed catalog |
| 37 | `51bf8be` | tray-i18n plan | DOC-ONLY | docs/plans/tray-i18n.md already lives outside v2's scope; preserve via cherry-pick |
| 38 | `301ad5f` | profile-unification plan | DOC-ONLY | v2-internal; drop |
| 39 | `b43446d` | rescope profile-unification | DOC-ONLY | v2-internal; drop |
| 40 | `2d99a34` | ProfileManager trait + profile_fields registry | DROP | trait gone; registry lives in GUI |
| 41 | `dadfe4d` | route mutations through ProfileManager | DROP | no trait, mutations are GUI → `Manager1.UpdateSystemProfile` |
| 42 | `cc9111e` | IdleTimeoutSeconds spinner + field-kind dispatch | CARRY-IN-SPIRIT | dispatch carries to v3 GUI; the SpinBox + Number row land naturally |
| 43 | `b32686c` | shave LoC from refactor | DROP | cleanup of v2-only commits |
| 44 | `c0e8927` | trim dead code, hoist validators | DROP | cleanup of v2-only commits |

Counted from the table: 18 CARRY, 3 CARRY-PARTIAL, 6 RE-IMPLEMENT, 12 DROP, 5 DOC-ONLY = 44. The CARRY-PARTIAL rows are the wire-coupled UI commits (5, 14, 15) that need their daemon-side data source rewired even though the visual / state-shaping logic ports verbatim.

## v3 implementation sequence

Eleven commits planned across five phases, each green on `just check`. Phase 2 holds the wire-independent UI polish (widgets, tokens, dialog primitives, layout) so visual review lands early; Phase 3 lands the daemon-side property-bag surface; Phase 4 splits the GUI rebind into three reviewable commits (8 = state.rs, 9 = editor + signal handler, 10 = dispatch + dialogs + spinner); Phase 5 migrates the architecture into `docs/design.md` and deletes the plan.

A handful of v2 commits look like Phase 2 UI polish but actually call the v2 `list_*_profiles_full` ObjectManager-walking helpers — they need v3's `Manager1.ListSystemProfiles` to compile. Those (commits 5, 14, 15, 27) defer to Phase 4 even though their visual changes are pure polish.

### Phase 1 — plan (commit 1)

**Commit 1** — `docs(plans): profile-CRUD v3 — property-bag wire surface`. This file. An earlier sketch of this phase included a `zbus` 5.11+ dep bump for nested-dict support; verified against a round-trip fixture that the workspace's current 5.x pin handles the body shape, so the dep stays as-is.

### Phase 2 — wire-independent UI polish (commits 2–5)

**Commit 2** — `feat(gui): widget + icon additions for Profiles pane`. Bundle:

- `widgets.slint`: promote `ConfirmDialog` from `tvs_pane`; add `NameInputDialog`; add `LinkText`; add `ProfileTile`.
- `globals/icons.slint`: add `im_user`, `system_users`, `copy`, `delete` glyphs + their `load_themed_icon` calls.

(Carries: 23, 26, plus the widget portion of 7 and 31.)

**Commit 3** — `feat(gui): design tokens for Profiles ↔ TVs parity`. `design.slint` gains `pad-header-top: 18px`, `pad-body-top: 20px`, `gap-meta: 10px`. (Carries: token portion of 31.)

**Commit 4** — `feat(gui): Profiles-pane Slint shell — master list + detail header`. Visual shape only — wires onto a Rust-side `Profiles` global whose data fields are stubbed empty. The detail body is a placeholder until Phase 4 lands the editor binding.

- `globals/profiles.slint`: `Profiles` global with `session-available` field (carries commit 13's session-available sync), `ProfileKind { System, User }` enum, `ProfileRow` + `ProfileTvRef` + `ProfileFieldEntry` structs.
- `ProfileFieldEntry` struct lands with **every field enumerated up front**, including the `kind` discriminator (`enum`, `number`, `bool`, `text`), the enum-row variant's `options` list, and the number-row variant's `min_value` / `max_value` / `step` / `unit` fields. Even though commit 9 is the first consumer, Slint generates Rust types at `include_modules!` time — retroactively growing the struct in commit 9 would force a Slint reload mid-sequence. Field rationale stays in the comment.
- `components/profiles_pane.slint`: full shell with stacked sections, `ProfileListRow` (tile + name + status), `ProfileSectionHeader`, `ProfileDetailHeader` 3-column shape with Flickable Used-by ticker, status badges, action buttons stub, FIELDS title row with reset-all placement, top-aligned body, 50/50 split, 64px row height.
- Rust-side: empty `Profiles` global push from `lib.rs`. The shell's callbacks (`select`, `value-edited`, `value-edited-number`, `reset-field`, `reset-all`, `confirm-create`, `confirm-delete`, `confirm-duplicate`, `rename-selected`) all wire to no-op placeholder closures in `lib.rs` so the Slint shell compiles. Commit 10 replaces those with real dispatch.
- `state.rs` additions defer to commit 8 because they depend on the new `Manager1.ListSystemProfiles` shape.

(Carries: 20, 21, 22, 28, 29, 30, 32, 33, 34, plus the shell portion of 7 and the session-available portion of 13.)

**Commit 5** — `feat(gui): Errors.profiles slot + a11y on Profiles pane`. (Carries: 11. Session-bus signal-routing detail defers to Phase 4 commit 8.)

### Phase 3 — daemon + session property-bag surface (commits 6–7)

**Commit 6** — `feat(daemon,core): SystemProfileBody type + Manager1 CRUD methods + references_of helper + CLI wiring`.

Checklist:

- `tisvlay-core::config`: add the `Type` derive companion + an explicit string-signature attribute (`signature = "s"`) on `SessionAction` and `AutomateWhen` enums. Without these the dict-derive macro can't compute the body's enum-field variant signature. Lock the unit-variant-only invariant with a `// SAFETY:` style comment per the "Body types" section.
- `tisvlay-core`: `SystemProfileBody` `#[derive(Type, SerializeDict, DeserializeDict)]` struct with `signature = "a{sv}", deny_unknown_fields` (per "Body types" section above) + lossless `From<SystemProfileConfig>` / `Into<SystemProfileConfig>` (existing `Option<T>` shape lines up).
- `tisvlay-core`: optional `body_version: Option<u64>` inert field on the body — server emits `Some(monotonic_counter)` on every successful `Update`, clients ignore it. Deferred ETag-style concurrency tracking is added inert now so it can become load-bearing later without a wire break. See risks section.
- `tisvlay-core`: `references_of(uuid, &[TvAssignment]) -> Vec<Uuid>` helper encoding the fan-out rule. Mirror `user_references_of`.
- `tisvlay-core::dbus_client`: typed `DaemonClient::*_system_profile` wrappers (per "Client API").
- `tisvlayd::dbus::manager`: eight methods (`List*`, `Get*`, `Update*`, `Create*`, `Duplicate*`, `Delete*`, `GetResolvedSystemProfileDefaults`, `GetEffectiveSystemProfile`) plus three signals (`SystemProfileChanged(uuid)`, `SystemProfileAdded(uuid)`, `SystemProfileRemoved(uuid)`). Rename folds into `Update` per "Wire surface" — no dedicated method. `Added` fires once after persistence for both Create and Duplicate (Duplicate's body is the source's copy; clients fetch on `Added` without waiting for `Changed`).
- `tisvlayd::tv_manager`:
  - `Create` / `Delete` / `Duplicate` (carry the validation rules from v2 commit 6).
  - `Update` — full-body atomic replace. Name validation (non-empty, unique, default-name rules) gates the write; `body_version` increments under the write lock.
  - `ensure_default` materialization (carry v2 commit 36's invariant; UUID-keyed).
  - Persistence-failure semantics for all mutators (`Update` / `Create` / `Duplicate` / `Delete`): daemon validates → mutates in-memory state under the write lock → persists via atomic-rename (`save_to` writes a temp file + renames). If persistence fails, the in-memory mutation is rolled back to its pre-mutation snapshot, no signal fires, and the D-Bus method returns `org.tisvlay.Error.IoError` with the underlying error text. Tests cover this for at least `Update` (simulate a read-only config dir).
- `tisvlay-cli`: UUID args for `tisvlay profile {show,set,delete}`; display-name resolution via `Manager1.ListSystemProfiles` for the menu output. (~50 LoC; budget allocated in footprint table.)
- 13 integration tests: list / get / update / create / duplicate / delete / inherit-fallthrough / reference-blocks-delete / persistence-failure / rename-via-update / unknown-key-rejected / `SystemProfileChanged` signal contract / `SystemProfileAdded` + `SystemProfileRemoved` signal contracts. Also adapts existing `profile_dispatch.rs` and `reload_resolves_system_profile.rs` tests to the new wire shape.

**Commit 7** — `feat(session,core): UserProfileBody type + Session1 CRUD methods + user_references_of helper`. Mirror of commit 6 on the session bus. 13 integration tests. **Plus**: cross-bus signal subscription — the session daemon subscribes to `Manager1.SystemProfileChanged(uuid)` for the admin-baseline UUIDs (the system-config user-default + admin-baseline named profile referenced from `[user.profiles]`) and re-fires `Session1.UserProfileChanged(uuid)` for every session-side user profile whose resolution depends on those baselines. See "User-side cross-bus gap" in the inheritance-signaling section above. Test: mutate the admin baseline via `Manager1.UpdateSystemProfile`, verify `Session1.UserProfileChanged` arrives for inheriting profiles.

### Phase 4 — GUI rebind (commits 8–10)

Three commits split from a single ~1 800 LoC monolith — each leaves the GUI in a coherent intermediate state.

**Commit 8** — `feat(gui): state.rs + state_watch.rs rebind to property-bag list + references_of`.

- `state.rs::convert_snapshot` re-points the master-list source from v2's ObjectManager walk to `Manager1.ListSystemProfiles` + `Session1.ListUserProfiles`. Composes `ProfileRow` from `(uuid, body.name, references_of(uuid, &tvs))`.
- New `StateEvent::SystemProfileChanged(Uuid)` + `StateEvent::SystemProfileAdded(Uuid)` + `StateEvent::SystemProfileRemoved(Uuid)` variants (mirror for user).
- `state_watch.rs` subscribes to `Manager1.SystemProfileChanged` + `SystemProfileAdded` + `SystemProfileRemoved` (system bus) and the user-side mirrors (session bus). Refreshes the master list on Add/Remove, refreshes one body on Changed.
- Includes the body-fetch primitive (`fetch_system_profile_body(uuid) -> SystemProfileBody`) the next commit will build on. Exercised by an explicit unit test in commit 8 to avoid `clippy -D warnings` flagging it as dead code before commit 9 consumes it.
- Test additions: `convert_snapshot` regression on the new shape, including the references fan-out semantic; dedicated `references_of` unit tests in `tisvlay-core` covering the default-implicit + explicit-assignment cases for both system and user variants.
- Adapt `crates/tisvlay-harness/tests/gui_pair_flow.rs` to the new profile state shape — v2's commit `cbf9ff3` touched this file and v3's commit 8 will need a similar rewire.

(Carries: 5 (its wire-coupled parts), 14, 15, 27.)

**Commit 9** — `feat(gui): profiles.rs editor rebuild — ProfileLayers cache + signal-driven row refresh`.

- New `crates/tisvlay-gui/src/profiles.rs` editor state: `ProfileLayers { baseline, default_body, bodies: HashMap<Uuid, Body> }`. One per kind (system + user).
- Resolution walker — `resolve_field(layers, uuid, field) -> ResolvedValue` — encodes the inheritance chain (system: 3 layers; user: 5 layers, with the admin baseline cross-bus dependency wired through the daemon client).
- `select(kind, uuid)` primes the layers if not already cached and triggers a row rebuild.
- Signal handler consumes `StateEvent::SystemProfileChanged(uuid)` from commit 8 and refreshes only the affected row(s). Selection-equality check protects against stale callbacks during reselection (replaces v2's generation-counter pattern).
- Field-kind registry lives in this module (not `tisvlay-core`).
- `ProfileFieldRowEnum` / `ProfileFieldRowNumber` Slint components (carry v2 commit 42's shape) wired to the new model.
- Inherit-placeholder `<resolved>*` convention (carry v2 commit 33); ComboBox dropdown lists concrete values only (carry v2 commit 34); reset glyph is the sole revert path.

(Carries: 8, 42 — the kind dispatch + spinner.)

**Commit 10** — `feat(gui): dispatch helpers + rename UX + duplicate + spinner + dialog wires`.

- Edit dispatch helpers (`dispatch_value_edit`, `dispatch_value_edit_number`, `dispatch_reset_field`, `dispatch_reset_all`, `dispatch_rename`) all clone the cached body, patch one field (or zero many), call `update_system_profile(uuid, body)`. Per-field None-setter (a small per-kind `body.set_none(field_wire_name)` helper) replaces v2's `ResetField` per-property method.
- Profile rename UX on detail header (carry v2 commit 9 visual + wire to a name-only body update via `dispatch_rename`).
- Profile duplication via `NameInputDialog` (carry v2 commit 19 visual + wire to `Manager1.DuplicateSystemProfile`).
- IdleTimeoutSeconds spinner ships here (the daemon-side numeric field already lives on `UserProfileBody` from commit 7; this commit wires the GUI dispatch).
- `lib.rs`: wire all Profiles callbacks (select, confirm-create, confirm-delete, confirm-duplicate, rename-selected, value-edited, value-edited-number, reset-field, reset-all) onto the dispatch helpers above.

(Carries: 9, 19, 42's dispatch portion.)

### Phase 5 — durable docs (commit 11)

**Commit 11** — `docs: migrate v3 architecture into design.md + i18n catalog refresh + delete plan`.

- `docs/design.md`: Components row marks Profiles pane built. Manager1 / Session1 sections updated with the v3 CRUD methods + body-dict types + signal trio. New "Profile catalog" subsection covers the CRUD surface table, the inheritance-chain layers, the editor's instant-commit model, and the live-refresh signal contract. TOML examples keep the v2 UUID-keyed `[system.profiles."<UUID>"]` shape with `name = "..."` as a body field.
- `just i18n-extract` + `just i18n-update` — refresh `crates/tisvlay-gui/lang/tisvlay-gui.pot` and merge into existing per-locale `.po` files for every new user-facing string introduced across Phase 2 + Phase 4 commits (dialog copy, helper text, error toasts, FIELDS labels, etc.). Single-shot at the tail keeps the per-commit signal-to-noise high.
- `docs/plans/profile-crud-v3.md` (this file): deleted, per `docs/plans/README.md` convention.

## What this gets us

### Estimated v3 footprint

Working budget against `origin/main`, with v2-equivalent file deltas measured against `feat/profile-crud-v2`:

| Component | v3 budget | v2 actual (equivalent files) | Notes |
|---|---:|---:|---|
| `tisvlay-core` body types + `references_of` + dbus_client wrappers | ~500 | ~530 (config +240 - profile-fields +82 - profile-manager +35 + dbus_client +378) | Wrappers, UUID-keyed config, dict-derive companions; v2 had `profile_fields` + `profile_manager` overhead that v3 drops, but the wrapper surface is comparable. |
| `tisvlayd` Manager1 + TvManager + CLI | ~700 | ~700 (manager +74, tv_manager +615 catalog ops, cli +9) | v3 adds `Update` atomic-replace + `body_version` increment; v2 had `system_profile.rs` + `state_forwarder` fan-out that v3 drops entirely (~330 LoC saved). |
| `tisvlay-session` Session1 + helpers + cross-bus signal | ~700 | ~800 (session_dbus +107, user_profile +798 minus per-property forwarding) | Mirrors daemon catalog CRUD + idle-timeout numeric; cross-bus signal subscription adds ~50 LoC. |
| GUI Slint shell (master + detail + dialogs) | ~1 600 | ~1 627 (profiles_pane +1 129, profiles global +205, widgets +272, icons/design +21) | Slint markup verbosity is a constant — same visual shape carries from v2 at the same LoC. |
| GUI Rust state.rs + state_watch + harness adapt (commit 8) | ~700 | ~530 (state +371, state_watch +152) + ~80 references-walker | v3 adds `references_of` reverse-walk + body-fetch primitive + `references_of` unit tests + harness adapt that v2 didn't isolate. |
| GUI Rust editor cache + walker + signal handler (commit 9) | ~700 | ~774 (profiles.rs) | v2 ran a per-profile proxy + generation-counter; v3 substitutes a 3-layer cache + walker, comparable in scope. |
| GUI Rust dispatch + dialogs + spinner (commit 10) | ~600 | ~524 (lib.rs dispatch wiring) | Per-field-kind dispatch tables + 4 dialog wires + spinner. |
| Widget + icon + token additions | ~250 | ~293 | Carries verbatim. |
| Tray updates (UUID-keyed menu items, display names) | ~100 | ~38 | v2 actual lower than budget — headroom. |
| Integration tests | ~2 500 | ~3 144 (32 files × ~98 LoC/test) | v2's per-test floor is ~98 LoC of harness boilerplate (private-bus setup, mock registration); v3 targets ~30 tests at the same floor. |
| Plan doc (this file, deleted on merge) | ~750 | n/a | |
| **Total** | **~9 100** | **+12 646 v2 total** | |

vs v2's +12 646 LoC. **Estimated savings: ~3 500 LoC**, same user-level features + the spinner.

The footprint estimate is higher than the earlier ~6 800 sketch because measuring v2-equivalent files surfaced floors the previous estimate missed: Slint markup verbosity (~1 600 LoC for the visual shape, regardless of wire surface), per-test harness boilerplate (~98 LoC/test), and dbus_client wrapper surface area. The architectural win — dropping per-profile D-Bus object infrastructure + per-property typed properties + state-forwarder cross-profile fan-out — still saves ~3 500 LoC against v2's ceiling, and the wire-shape simplification is the actual prize, not the LoC delta.

### User-visible features parity with v2

All of:

- Master/detail Profiles pane with stacked System / User sections.
- Create / Rename / Delete / Duplicate per profile, via dialogs + name-input + delete-confirm-with-reference-list.
- Per-TV profile assignment.
- Inheritance with locally-set indicator (gutter accent), per-field reset glyph, reset-all.
- IdleTimeoutSeconds spinner (numeric field).
- Detail-header rename pencil.
- Used-by chip ticker with click-through to TVs pane.
- TVs ↔ Profiles pane visual parity (tiles, headers, padding tokens).
- Errors.profiles inline banner for reload failures.
- A11y (list-item / button roles + keyboard reach).
- Toasts for per-action errors.
- Reserved-default materialization.

Nothing user-visible regresses. The only architectural feature v2 had that v3 explicitly does not is **per-field PropertiesChanged granularity**. v3 substitutes per-body re-fetch on `SystemProfileChanged(name)`, which is one dict deserialize — cheaper than v2's per-property fan-out machinery and equivalent latency in practice. Verified at acceptance: same ≤ 50 ms perceived-latency target v2 set.

## Risks

- **TOML format compatibility.** v3 keeps v2's UUID-keyed `[system.profiles."<UUID>"]` shape on disk (same name field on the body), so configs that survived v2 development load cleanly into v3. `origin/main` is name-keyed, so a fresh install on top of `origin/main` will hit v3's "load fails on missing UUID key" path. **Mitigation**: v3's `SystemConfig::load_from` materializes the reserved-default UUID on startup if the section is absent (carries v2 commit 590bfde's invariant), so a `main`-shaped config gets quietly upgraded on first save. Old name-keyed `[[tv]] profile.system = "office"` entries fail to deserialize loudly — no auto-migration shim, mirroring v2's choice.

- **Dict-derive behavior round-trip.** The plan rides on `Option<T>::None` ⇔ absent key. Verified against the workspace's current 5.10 pin via a small fixture; the round-trip works. **Mitigation**: commit 6's test suite includes a round-trip test for `SystemProfileBody` + `UserProfileBody` that fails loudly if the absent-key ↔ `None` mapping diverges.

- **Concurrent body updates (read-modify-write is racy).** Two clients each clone-patch-send on the same UUID. Atomic-replace means last-write-wins on the *whole body* — fields the second client didn't touch but the first client did revert to the second's body. The realistic loss scenario isn't two GUI users; it's a scripted CLI loop (`for tv in ...; tisvlay profile set ...`) interleaved with an open GUI session whose cached body is seconds-stale. By the time the post-edit `SystemProfileChanged` signal arrives, the loss is already persisted to disk — the signal can't reconcile retroactively. **Mitigation**: the body's `body_version: Option<u64>` (commit 6) lands **inert** in v3 — server emits a monotonic counter on every `Update`, clients ignore it. If multi-client editing ever becomes a real workflow, `Update` grows an `expected_version: Option<u64>` arg + daemon-side compare-and-swap with a typed `org.tisvlay.Error.Conflict` rejection. Inert-now means the wire grows additively, without breaking the existing contract, when the concurrency feature ships.

- **Body validation surface.** `UpdateSystemProfile(uuid, body)` accepts an `a{sv}`. With `deny_unknown_fields` on the body struct (per "Body types" section), the dict-derive rejects unknown keys at the boundary and the typed-field deserializers reject wrong-typed values. The daemon still has to reject domain-invalid values (e.g. `automate_when = "always_but_only_on_tuesdays"`); `SystemProfileConfig` already has serde-enforced enums, and `From<SystemProfileBody>` → `SystemProfileConfig` inherits that. **Mitigation**: commit 6's `unknown-key-rejected` test pins the strict-mode behavior; a sibling `invalid-enum-value` test pins domain validation.

- **Cross-client name collision with localized "default".** A non-English-locale user could create a profile whose name happens to be the localized form of "default" (the gettext catalog's translation of the reserved name). Server only rejects the literal English string `"default"`. The localized form would visually collide with the reserved-default profile in that user's GUI. **Mitigation**: rename / create dialogs do a client-side `gettext("default")` lookup and reject the localized form too, with a daemon-side fallback that catches the English form. Documented in the dialog validation code.

- **i18n collision risk on display labels generally.** Display-name uniqueness is enforced on the wire by the daemon, but localized renderings of "default" + arbitrary user-typed names can still visually collide (a profile named `"Standard"` next to the localized default in German). Out of scope for v3 — the GUI's `(default)` badge disambiguates the reserved one.

- **Branch divergence from v2.** v2 stays on `feat/profile-crud-v2` for archaeology + comparison; v3 ships from `feat/profile-crud-v3` against `main`. If we ever want to revisit v2 (e.g. for a future feature where per-property granularity actually matters), the branch + the v2 plan history remain.

## Acceptance criteria

- `just check` end-to-end green at every commit.
- `prek run -a` end-to-end green at every commit.
- Test count: existing 380-something baseline plus ~30 new v3 integration tests (13 daemon + 13 session + ~4 in `tisvlay-core` for the dict-derive round-trip + `references_of` fan-out unit tests) = ~410 passing.
- Manual GUI smoke test:
  - Profile create flow (System + User).
  - Profile rename flow (default + non-default).
  - Profile duplicate flow.
  - Profile delete flow (default rejected, referenced rejected, unreferenced succeeds).
  - Per-field edit + reset + reset-all on both kinds.
  - IdleTimeoutSeconds spinner edit + reset.
  - Cross-client live refresh (edit via a scripted D-Bus client, watch GUI update without manual reload).
  - Reload-error inline banner surfaces visibly.
- Code-LoC delta against `origin/main` reported in the final commit message — target ≤ +9 000 code LoC across Rust + Slint (excluding the plan doc, which is deleted on merge).
- Per-edit perceived latency ≤ 50 ms (carry v2's bar).

## Durable docs migration on merge

Per `docs/plans/README.md` convention, when this work merges:

- `docs/design.md` grows the v3 Manager1 / Session1 surface + body-dict reference + inheritance subsection (per the commit 11 sketch above).
- This plan file is deleted; git history is the archive.
- v2's `docs/plans/profile-unification.md` was already deleted by v2's plan migration; no v2 plan artifacts remain in tree.
- The architectural lesson worth preserving: "configuration over D-Bus is property-bag-shaped, not typed-property-per-object — the BlueZ pattern works for operational state, not for configuration." This goes into `docs/design.md`'s introduction of the Manager1 surface as a one-paragraph rationale, citing the FOSS survey indirectly.

## Sequencing rationale

Phase 1 (plan only) lands first so the architectural rationale is in tree before any code lands; reviewers reading subsequent diffs have the design doc to anchor against.

Phase 2 (UI cherry-picks) lands second because the polish work is wire-independent — widgets, tokens, dialog primitives, Slint shell. Visual review happens early; daemon + GUI rebind in phases 3–4 land against a known-good visual baseline. Four v2 commits (5, 14, 15, 27) that *look* like Phase 2 polish actually depend on `Manager1.ListSystemProfiles`; they defer to Phase 4.

Phase 3 (daemon + session CRUD surface) lands before Phase 4 (GUI rebind) because GUI integration tests need the daemon surface to dispatch against. Commits 6 and 7 are paired by bus side but each self-contained; either could land first if a reviewer pipeline demanded it.

Phase 4 (GUI rebind) splits into three commits — state.rs / editor / dispatch — because the v2-monolith equivalent was ~1 800 LoC. The split lines run along boundaries that leave the GUI in a coherent state at each commit: commit 8 lands the new list source + signal plumbing (master list works, editor still placeholder); commit 9 lands the editor cache + resolution walker + row refresh (fields render with inheritance); commit 10 lands dispatch helpers + rename / duplicate / spinner UX (writes work). Splitting any further would force back-references to a not-yet-landed module.

Phase 5 (docs migration) is the conventional plan-doc cleanup — migrate the durable architecture into `docs/design.md` and delete this plan, per `docs/plans/README.md` convention. Same shape as v2's commit 35.

## Cross-references

- v1 PR: <https://github.com/ruffsl/TisVlay/pull/58>
- v2 PR: <https://github.com/ruffsl/TisVlay/pull/59>
- FOSS prior-art survey synthesizing why property-bag is the idiomatic shape for typed configuration: searchable via git log on `docs/plans/profile-unification.md` (now deleted) under v2 commits `301ad5f` + `b43446d`.
