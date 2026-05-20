# Profile-CRUD v3: property-bag wire surface

## Motivation

This is the third attempt at landing profile management. The first two are instructive context, not noise — every architectural choice in v3 traces back to a lesson from v1 or v2.

**v1** (PR #58, `feat/profile-crud`, 13 commits, +4 947 / −131 LoC, 28 files): whole-body JSON CRUD on `Manager1` / `Session1`. `Get`/`Create`/`Update`/`Delete` carrying a JSON-serialized `SystemProfileConfig` plus `GetResolvedProfileDefaults` for the inheritance baseline. The GUI computed three-layer inheritance client-side (`profiles_state.rs`) and refetched all three layers on every field edit + rebuilt the Slint model. **Rejected** during smoke testing because the per-edit latency was visible to the user — every ComboBox click produced a JSON parse, an `UpdateProfile` round-trip, three further refetches, and a full model rebuild.

**v2** (PR #59, `feat/profile-crud-v2`, 44 commits, +12 646 / −2 077 LoC, 86 files): lifted profiles to first-class D-Bus objects under `/org/tisvlay/{System,User}Profile/<UUID_>` with one typed `#[zbus(property)]` per field, mirroring the per-TV `Tv1.*` pattern. Field edits become single `Properties.Set` round-trips. `LocallySetFields: as` companion property carries the inheritance status; `ReferencedBy: as` lists referencing TV UUIDs; per-profile object adds `ResetField` + `ResetAll` methods. UUID-keyed both on disk and on the wire so rename doesn't have to walk and rewrite references.

v2 fixed the responsiveness problem but at a structural cost that grew through the implementation:

- 3 000 LoC of per-profile D-Bus object infrastructure (server-side interface impls + register / unregister + the `state_forwarder`'s cross-profile-fanout machinery).
- 3 000 LoC of integration tests across ~40 files exercising the per-property surface.
- 800 LoC of GUI per-profile proxy + listener + generation-counter machinery to consume the per-property `PropertiesChanged` granularity.
- 500 LoC of UUID migration touching every consumer (CLI, tray, GUI, configs).
- 600 LoC of `tisvlay-core` follow-up (`ProfileManager` trait + `profile_fields` registry) trying to consolidate the duplication that fell out of the structural choice.

After two rounds of cleanup the net delta against `origin/main` is +133 code lines, but only because most of the v2 commits added structural overhead the cleanup rounds then trimmed. The "more concise idiomatic code" goal failed at the architectural level.

**v1 was right that whole-body CRUD is the idiomatic shape** for configuration over D-Bus — `NetworkManager.Settings.Connection.Update(a{sa{sv}})`, `systemd-homed.Home.Update(s)`, `power-profiles-daemon`'s `Profiles: aa{sv}` all use property-bag bodies, not per-field typed properties. **v1 was wrong about how to make the GUI fast** — the three-layer refetch was unnecessary work, not a wire-protocol shortcoming. The hardcoded baseline never changes; the "default" profile body changes rarely; only the selected body ever needs refetching on an edit, and even then it's one round-trip, not three.

**v2 corrected the wrong thing** — it kept the typed wire surface as the GUI's data source (good) but rewrote the wire shape from configuration-idiomatic (property bag) to operational-state-idiomatic (typed-property-per-object). The result was fast but structurally over-engineered for what is fundamentally configuration data: 5–7 mostly-static fields per profile, 1–10 profiles per catalog.

**Unblocking development**: zbus / zvariant 5.11.0 (PR z-galaxy/zbus#1779, merged April 2026) added support for nested dictionaries in `#[derive(DeserializeDict, SerializeDict)]`. Pre-5.11 the property-bag pattern required hand-rolled `HashMap<String, OwnedValue>` marshalling on the client side, which was a real ergonomic tax. With 5.11+, the GUI can `#[derive(DeserializeDict)]` directly into a typed Rust struct — the wire is `a{sv}`, the types are concrete, no hand-rolled marshalling. **This is the missing piece that makes property-bag the right call now.**

This plan resets to v1's wire shape, fixes v1's GUI-side latency with a 50-line baseline cache + ~50-line GUI rebind, and carries forward every UI polish + dialog primitive + widget + token + bug fix that v2 contributed.

## v3 architecture

### Wire surface

Two interfaces grow CRUD methods + change signals. No per-profile D-Bus objects. No `ObjectManager` walking. No per-property `PropertiesChanged` granularity.

**`org.tisvlay.Manager1`** (system bus, daemon-owned):

```
ListSystemProfiles() -> a{sa{sv}}                   ; name → body dict
GetSystemProfile(name: s) -> a{sv}                  ; one body
UpdateSystemProfile(name: s, body: a{sv})           ; replace body atomically
CreateSystemProfile(name: s)                        ; insert empty body
RenameSystemProfile(old: s, new: s)                 ; atomic rewrite under daemon lock
DuplicateSystemProfile(source: s, new: s)           ; clone body under new name
DeleteSystemProfile(name: s)                        ; rejects "default" + referenced
GetResolvedSystemProfileDefaults() -> a{sv}         ; static baseline, cached client-side
GetEffectiveSystemProfile(tv_id: s) -> a{sv}        ; resolved-for-this-TV walk

signal SystemProfileChanged(name: s)                ; fired on Update/Rename
signal SystemProfileListChanged                     ; fired on Create/Delete/Duplicate/Rename
```

**`org.tisvlay.Session1`** (session bus, session-owned): mirror surface for user profiles + the existing `SetTvUserProfile` / `GetTvUserProfile` per-TV assignment methods (unchanged from main).

### Body types

`tisvlay-core` exports two `#[derive(Type, SerializeDict, DeserializeDict, Default, Clone, Debug)]` structs:

```rust
#[zvariant(signature = "a{sv}")]
pub struct SystemProfileBody {
    pub on_startup:    Option<SessionAction>,
    pub on_shutdown:   Option<SessionAction>,
    pub on_reboot:     Option<SessionAction>,
    pub on_no_session: Option<SessionAction>,
    pub on_sleep:      Option<SessionAction>,
    pub on_wake:       Option<SessionAction>,
    pub automate_when: Option<AutomateWhen>,
}

#[zvariant(signature = "a{sv}")]
pub struct UserProfileBody {
    pub on_session_lock:      Option<SessionAction>,
    pub on_session_unlock:    Option<SessionAction>,
    pub on_greeter_wake:      Option<SessionAction>,
    pub on_idle_timeout:      Option<SessionAction>,
    pub on_idle_resume:       Option<SessionAction>,
    pub idle_timeout_seconds: Option<u32>,
}
```

zvariant 5.11+ serializes `Option<T>::None` fields as **absent keys** in the dict, not as null entries. `Some(value)` → key present with the variant-wrapped value. This is exactly the inherit-vs-locally-set distinction v2 needed `LocallySetFields` to express: present ⇒ locally set, absent ⇒ inheriting. **`LocallySetFields` is no longer a separate wire concept.**

### Inheritance model

Resolution stays server-side, identical to v2:

- System: `this → default profile → hardcoded baseline (ResolvedSystemProfile::default())`
- User: `this → user default → admin baseline named profile → admin user default → hardcoded baseline`

The walker code in `SystemSection::resolve_profile_by_uuid` / `UserSection::resolve_profile_by_uuid` keeps working as-is once we re-key from UUID to name. `GetEffectiveSystemProfile(tv_id)` returns the fully-resolved body for one TV (the GUI does not need this directly — it has the layers cached — but the CLI uses it for `tisvlay profile show <tv>`).

The GUI cache:

```rust
struct ProfileLayers {
    baseline: SystemProfileBody,           // static — fetched once at GUI start
    default_body: SystemProfileBody,       // refetched on SystemProfileChanged("default")
    bodies: HashMap<String, SystemProfileBody>, // refetched on SystemProfileChanged(name)
}
```

Three layers, fetched on signal, not on every edit. v1's perf failure was refetching all three per edit — that's gone.

### Reference shape

TVs reference profiles by name, same as v1: `[[tv]] profile.system = "office"`, `[[tv]] profile.user = "evening"`. Rename works because:

1. Daemon takes the write lock once.
2. Updates the profile entry under its new key in the catalog.
3. Walks every `[[tv]]` entry in the same lock-held section and rewrites matching references.
4. Persists.
5. Drops lock, emits one `SystemProfileChanged(new_name)` + one `SystemProfileListChanged`.
6. Each affected TV's `Tv1.SystemProfile` property is refreshed (existing per-property signal contract on the per-TV interface, which v3 does not touch).

The "atomic walk-and-rewrite" cost v2 cited as motivation for UUIDs is one serialized in-memory operation per rename — not a per-edit cost, not a wire-protocol concern. UUIDs aren't needed.

### Why rename can stay name-keyed safely

The race v2 worried about — two clients editing the same profile under conflicting names — doesn't exist in practice:

- Single daemon, single write lock per config section.
- Renames are rare (manual user action).
- Reads happen between renames; clients re-resolve via signal.
- `Manager1.RenameSystemProfile(old, new)` is one atomic call; the daemon either succeeds (old name no longer exists, new name resolves) or returns `InvalidArgs` (new name duplicate, old name unknown).

The signal sequence on rename is `SystemProfileListChanged` (catalog topology changed) → clients refetch list. No client needs to track stable identifiers across renames at a finer granularity than that.

### Client API

`tisvlay-core::dbus_client` grows typed wrappers:

```rust
impl DaemonClient {
    pub async fn list_system_profiles(&self) -> Result<HashMap<String, SystemProfileBody>>;
    pub async fn get_system_profile(&self, name: &str) -> Result<SystemProfileBody>;
    pub async fn update_system_profile(&self, name: &str, body: &SystemProfileBody) -> Result<()>;
    pub async fn create_system_profile(&self, name: &str) -> Result<()>;
    pub async fn rename_system_profile(&self, old: &str, new: &str) -> Result<()>;
    pub async fn duplicate_system_profile(&self, source: &str, new: &str) -> Result<()>;
    pub async fn delete_system_profile(&self, name: &str) -> Result<()>;
    pub async fn get_resolved_system_profile_defaults(&self) -> Result<SystemProfileBody>;
    pub async fn get_effective_system_profile(&self, tv_id: &str) -> Result<SystemProfileBody>;
}
```

Mirror set on `SessionClient` for user profiles. `#[derive(DeserializeDict)]` does the marshalling; no `HashMap<String, OwnedValue>` boilerplate.

### Editor wire shape

The GUI's editor commits an edit by:

1. Cloning the cached body for the selected profile.
2. Patching the one field (`body.on_startup = Some(SessionAction::ScreenOff)` to set, `= None` to reset).
3. One `Manager1.UpdateSystemProfile(name, body)` round-trip.
4. Daemon validates + persists + fires `SystemProfileChanged(name)`.
5. GUI's signal handler refetches the body and refreshes the field rows.

One D-Bus round-trip per edit, exactly matching v2's per-property latency. The body refetch is one trivial dict deserialize — much cheaper than v2's per-property `Properties.Get` per row.

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
| 2 | `f45ae07` | profile UUIDs + per-profile D-Bus object scaffolding | DROP | name-keyed; no per-profile objects |
| 3 | `4701e2b` | typed properties on SystemProfile1 + cross-profile fanout | DROP | property-bag instead |
| 4 | `35255de` | typed properties on UserProfile1 + cross-profile fanout | DROP | property-bag instead |
| 5 | `77cfd56` | TV profile refs migrate to UUIDs | DROP | name-keyed refs stay |
| 6 | `d3a206d` | Manager1 + Session1 Create/Delete + drop list RPCs | RE-IMPLEMENT | broader Manager1 surface (Update/Rename/Duplicate too) |
| 7 | `b356834` | rebind Profiles pane master list to ObjectManager | RE-IMPLEMENT | bind to `Manager1.ListSystemProfiles` + `SystemProfileListChanged` |
| 8 | `774750f` | bind ProfileFieldRow to per-profile D-Bus properties | RE-IMPLEMENT | bind to cached body + `SystemProfileChanged` |
| 9 | `6afd4df` | rename UX on profile detail header | RE-IMPLEMENT | wire to `RenameSystemProfile` |
| 10 | `000139d` | show profile detail on same frame as click | CARRY | Slint global `in-out` fix; orthogonal |
| 11 | `56c06ee` | Errors.profiles banner + a11y | CARRY | Errors slot + a11y; signal-routing detail re-implemented |
| 12 | `f3a1d8a` | register UserProfile1 D-Bus objects after late daemon connect | DROP | no per-profile objects to register |
| 13 | `8b45e1a` | clear "Session service unavailable" on SessionAppeared | CARRY | session-available sync; orthogonal |
| 14 | `53fc607` | TVs-pane combobox display name not UUID | DROP | refs already name-keyed |
| 15 | `cbf9ff3` | route TVs-pane pencil → Profiles by UUID | DROP | route by name (simpler) |
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
| 27 | `91d8b46` | derive active state per ProfileRow | RE-IMPLEMENT | active = derived from `body.referenced_by_tvs.any(automated)`; computed against the same fetched body |
| 28 | `55744f5` | redesign master-list row in TVs shape | CARRY | layout shape; orthogonal |
| 29 | `0034ca9` | detail header 3-column shape | CARRY | layout shape; orthogonal |
| 30 | `7420668` | align detail-header height with TVs | CARRY | Flickable + plain-text spans; orthogonal |
| 31 | `a9fd81c` | tokenize header/body padding + LinkText | CARRY | tokens + widget; orthogonal |
| 32 | `7cceb52` | increase ProfileListRow height | CARRY | layout; orthogonal |
| 33 | `f4f9350` | inherit-placeholder is `<resolved>*` | CARRY | display convention; orthogonal |
| 34 | `22f09f6` | drop inherit-placeholder from ComboBox dropdown | CARRY | ComboBox shape; orthogonal |
| 35 | `a1c9fc1` | docs: migrate v2 + parity plans into design.md | DOC-ONLY | superseded; v3's design.md migration will be similar in shape |
| 36 | `590bfde` | materialize reserved-default profiles when TOML omits the section | RE-IMPLEMENT | same `ensure_default` invariant, name-keyed |
| 37 | `51bf8be` | tray-i18n plan | DOC-ONLY | docs/plans/tray-i18n.md already lives outside v2's scope; preserve via cherry-pick |
| 38 | `301ad5f` | profile-unification plan | DOC-ONLY | v2-internal; drop |
| 39 | `b43446d` | rescope profile-unification | DOC-ONLY | v2-internal; drop |
| 40 | `2d99a34` | ProfileManager trait + profile_fields registry | DROP | trait gone; registry lives in GUI |
| 41 | `dadfe4d` | route mutations through ProfileManager | DROP | no trait, mutations are GUI → `Manager1.UpdateSystemProfile` |
| 42 | `cc9111e` | IdleTimeoutSeconds spinner + field-kind dispatch | CARRY-IN-SPIRIT | dispatch carries to v3 GUI; the SpinBox + Number row land naturally |
| 43 | `b32686c` | shave LoC from refactor | DROP | cleanup of v2-only commits |
| 44 | `c0e8927` | trim dead code, hoist validators | DROP | cleanup of v2-only commits |

Counted: 19 CARRY, 6 RE-IMPLEMENT, 14 DROP, 5 DOC-ONLY. (Sums to 44 with overlaps: some commits are both CARRY for UI polish and partly RE-IMPLEMENT for the wire-bound side.)

## v3 implementation sequence

Ten commits planned, each green on `just check`. Sequence prefers UI-polish-first (cherry-picks) because they're independent of the wire surface and let us share visual review early; daemon + GUI rebind comes after.

### Phase 1 — scaffolding (commits 1–2)

**Commit 1** — `docs(plans): profile-CRUD v3 — property-bag wire`. This file.

**Commit 2** — `chore(deps): pin zvariant 5.11+ for nested DeserializeDict`. Bumps workspace `Cargo.toml`'s `zbus` / `zvariant` minimum to 5.11.0; `cargo update` regenerates `Cargo.lock`. Verifies via a small `#[derive(DeserializeDict)]` test in `tisvlay-core` that nested `Option<T>` body shape round-trips.

### Phase 2 — UI polish cherry-picks (commits 3–7)

Roughly 14 of the v2 commits are CARRY; many can be combined per the "larger chunks" preference. Some will conflict against the missing per-profile-object code paths — for those, hand-apply the visual / token / widget pieces while skipping the wire-bound bits (the wire-bound bits land in Phase 4).

**Commit 3** — `feat(gui): widget + icon additions for Profiles pane`. Bundle:

- `widgets.slint`: promote `ConfirmDialog` from `tvs_pane`; add `NameInputDialog`; add `LinkText`; add `ProfileTile`.
- `globals/icons.slint`: add `im_user`, `system_users`, `copy`, `delete` glyphs + their `load_themed_icon` calls.

(Carries: 7-partial, 23, 26, 31-partial.)

**Commit 4** — `feat(gui): design tokens for Profiles ↔ TVs parity`. `design.slint` gains `pad-header-top: 18px`, `pad-body-top: 20px`, `gap-meta: 10px`. (Carries: 31-partial.)

**Commit 5** — `feat(gui): Profiles-pane Slint shell — master list + detail header`. The visual shape only — wires onto a Rust-side `Profiles` global whose data fields are stubbed empty. The detail body is a placeholder until commit 8 lands the editor binding.

- `globals/profiles.slint`: `Profiles` global, `ProfileKind` enum, `ProfileRow` + `ProfileTvRef` + `ProfileFieldEntry` structs.
- `components/profiles_pane.slint`: full shell with stacked sections, `ProfileListRow` (tile + name + status), `ProfileSectionHeader`, `ProfileDetailHeader` 3-column shape with Flickable Used-by ticker, status badges, action buttons stub, FIELDS title row with reset-all placement, top-aligned body, 50/50 split, 64px row height.
- Rust-side: empty `Profiles` global push from `lib.rs`; `state.rs` extended with `ProfileRowRaw` + `TvRefRaw` + the `active` boolean + `referenced_by_summary` String.

(Carries: 7-partial, 20, 21, 22, 27-partial, 28, 29, 30, 32, 33, 34.)

**Commit 6** — `feat(gui): Errors.profiles slot + a11y on Profiles pane`. (Carries: 11.)

**Commit 7** — `fix(gui): show profile detail on same frame as click + session-available sync`. (Carries: 10, 13, 16.)

### Phase 3 — daemon-side property-bag surface (commits 8–9)

**Commit 8** — `feat(daemon,core): SystemProfileBody type + Manager1 CRUD methods`.

- `tisvlay-core`: `SystemProfileBody` `#[derive(Type, SerializeDict, DeserializeDict)]` struct + `From<SystemProfileConfig>` / `Into<SystemProfileConfig>` (lossless, since `SystemProfileConfig` already has `Option<T>` fields).
- `tisvlayd::dbus::manager`: nine methods (`List*`, `Get*`, `Update*`, `Create*`, `Rename*`, `Duplicate*`, `Delete*`, `GetResolvedSystemProfileDefaults`, `GetEffectiveSystemProfile`) plus the two signals (`SystemProfileChanged`, `SystemProfileListChanged`).
- `tisvlayd::tv_manager`: 
  - `Create` / `Delete` / `Duplicate` (carry the validation rules from v2 commit 6).
  - `Rename` (new — atomic in-memory swap + `[[tv]]` reference rewrite under the existing write lock).
  - `Update` (replaces body atomically).
  - `ensure_default` materialization (carry v2 commit 36's invariant; re-keyed to name).
- `tisvlay-core::dbus_client`: typed `DaemonClient::*_system_profile` wrappers.
- 8 integration tests cover: list / get / update / create / rename / duplicate / delete / inherit-fallthrough / reference-blocks-delete / `SystemProfileChanged` signal contract.

**Commit 9** — `feat(session,core): UserProfileBody type + Session1 CRUD methods`. Mirror of commit 8 on the session bus. 8 integration tests.

### Phase 4 — GUI rebind (commit 10)

**Commit 10** — `feat(gui): Profiles pane wired to property-bag CRUD + spinner`.

- `tisvlay-gui/src/profiles.rs`: ~250 LoC editor state owning the three-layer cache (`baseline` + `default_body` + `bodies: HashMap<String, Body>`).
- `state_watch.rs`: subscribe to `SystemProfileChanged(name)` (system bus) + `UserProfileListChanged` + `SystemProfileListChanged`; refetch the affected body or the full list. Carry the session-bus signal-routing from v2 commit 7.
- `lib.rs`: wire the Profiles callbacks (select, confirm-create, confirm-delete, confirm-duplicate, confirm-rename, value-edited, value-edited-number, reset-field, reset-all) onto `DaemonClient` / `SessionClient` methods.
- Field-kind registry + `ProfileFieldRowEnum` / `ProfileFieldRowNumber` Slint components + dispatch (carry v2 commit 42). Registry lives in `tisvlay-gui/src/profiles.rs`, not `tisvlay-core`.
- Inherit-placeholder = `<resolved>*` (carry v2 commit 33); ComboBox dropdown lists concrete values only (carry v2 commit 34); reset glyph is the sole revert path.
- IdleTimeoutSeconds spinner ships in this commit (carry v2 commit 42).
- Profile rename UX on detail header (carry v2 commit 9 visual + wire to `RenameSystemProfile`).
- Profile duplication via `NameInputDialog` (carry v2 commit 19 visual + wire to `DuplicateSystemProfile`).

This is the biggest commit; ~1 200 LoC GUI Rust + ~150 LoC Slint adjustments. Splitting further would orphan visual surfaces from their wire bindings, which is what triggered the v2 cleanup churn.

### Phase 5 — durable docs (commit 11)

**Commit 11** — `docs: migrate v3 architecture into design.md + delete plan`.

- `docs/design.md`: Components row marks Profiles pane built. Manager1 / Session1 sections updated with the v3 CRUD methods + body-dict types + two signals. New "Profile catalog" subsection covers the CRUD surface table, the inheritance-chain layers, the editor's instant-commit model, and the live-refresh signal contract. TOML examples switch to name-keyed `[system.profiles.<name>]`.
- `docs/plans/profile-crud-v3.md` (this file): deleted, per `docs/plans/README.md` convention.

## What this gets us

### Estimated v3 footprint

Working budget against `origin/main`:

| Component | Approx LoC |
|---|---|
| `tisvlay-core` body types + dbus_client wrappers | ~250 |
| `tisvlayd` Manager1 methods + TvManager helpers | ~600 |
| `tisvlay-session` Session1 methods + helpers | ~500 |
| GUI Slint shell (master + detail + dialogs) | ~900 |
| GUI Rust adapter (editor cache + dispatch) | ~400 |
| GUI Rust callbacks + state.rs additions | ~250 |
| Widget + icon + token additions | ~250 |
| CLI updates (name-keyed args) | ~50 |
| Tray updates (name-keyed menu items) | ~100 |
| Integration tests | ~1 500 |
| Plan doc (this file, deleted on merge) | ~700 |
| **Total** | **~5 500** |

vs v2's +12 646 LoC. **Estimated savings: ~7 100 LoC**, same user-level features + the spinner.

This is the same architectural delta the FOSS survey identified earlier and the same "Option A: property-bag methods on Manager1/Session1" recommendation that surfaced after we read v2's commit messages. The estimate is conservative — actual could come in lower since v2's per-property test surface (~3 000 LoC) was disproportionate to its impact (we'll have ~1 500 LoC of tests in v3 for the same surface area).

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

Nothing user-visible regresses. The only architectural feature v2 had that v3 explicitly does not is **per-field PropertiesChanged granularity**. v3 substitutes per-body refetch on `SystemProfileChanged(name)`, which is one dict deserialize — cheaper than v2's per-property fan-out machinery and equivalent latency in practice. Verified at acceptance: same ≤ 50 ms perceived-latency target v2 set.

## Risks

- **Migration UX from v2's UUID-keyed TOML.** If we have any local test rigs or in-house configs that already moved to v2's UUID-keyed `[system.profiles."<UUID>"]` shape, v3 won't load them. **Mitigation**: v3 ships against `origin/main`'s name-keyed format; v2's UUID-keyed configs only existed in working trees, not in any deployed install. The repo's `docs/configs/daemon.toml` example reverts to the name-keyed shape.

- **zvariant 5.11+ derive behavior for `Option<T>` in `DeserializeDict`.** PR z-galaxy/zbus#1779 explicitly added nested-dict support, but our use case (top-level `Option<T>` fields, no nested dicts) was already working pre-5.11 for simple types. **Mitigation**: commit 2 adds a round-trip test for `SystemProfileBody` + `UserProfileBody` that fails loudly if the absent-key ↔ `None` mapping diverges from expectation. We need 5.11+ specifically for `idle_timeout_seconds: Option<u32>` — pre-5.11 had a known bug with `Option<u32>` in `DeserializeDict` (issue #312 / #1777 lineage).

- **Rename atomicity under concurrent reads.** A client doing `ListSystemProfiles` concurrent with a rename could see a transient state where the old name no longer exists. **Mitigation**: the daemon's write lock prevents concurrent `Update` + `Rename`; the read of `ListSystemProfiles` doesn't take the write lock and could race. In practice this is fine — the client gets a consistent snapshot of whichever side of the rename the read fell on, and the post-rename `SystemProfileListChanged` signal will trigger a refetch.

- **Body validation surface.** `UpdateSystemProfile(name, body)` accepts an arbitrary `a{sv}`. zvariant's `DeserializeDict` will reject unknown keys and wrong-typed values at the boundary, but the daemon should also reject domain-invalid values (e.g. `automate_when = "always_but_only_on_tuesdays"`). **Mitigation**: `SystemProfileConfig` already has serde-enforced enums; the `From<SystemProfileBody>` → `SystemProfileConfig` step inherits that. Add an explicit test that `UpdateSystemProfile` returns `InvalidArgs` on a malformed value.

- **GUI rebind volume in commit 10.** ~1 500 LoC in one commit is large. **Mitigation**: it's the *minimum* coherent chunk — the wire-binding side and the visual-shell side from commit 5 have to land together for the pane to work. Splitting further would create commits where the pane is half-bound and breaks. Reviewable via the test suite + manual smoke test.

- **Branch divergence from v2.** v2 stays on `feat/profile-crud-v2` for archaeology + comparison; v3 ships from `feat/profile-crud-v3` against `main`. If the user ever wants to revisit v2 (e.g. for a future feature where per-property granularity actually matters), the branch + the v2 plan history remain.

## Acceptance criteria

- `just check` end-to-end green at every commit.
- `prek run -a` end-to-end green at every commit.
- Test count: existing 380-something baseline plus ~25 new v3 integration tests = ~405 passing.
- Manual GUI smoke test:
  - Profile create flow (System + User).
  - Profile rename flow (default + non-default).
  - Profile duplicate flow.
  - Profile delete flow (default rejected, referenced rejected, unreferenced succeeds).
  - Per-field edit + reset + reset-all on both kinds.
  - IdleTimeoutSeconds spinner edit + reset.
  - Cross-client live refresh (edit via `busctl`, watch GUI update without manual reload).
  - Reload-error inline banner surfaces visibly.
- `tokei` delta against `origin/main` reported in the final commit message — target ≤ +6 000 code LoC across Rust + Slint.
- Per-edit perceived latency ≤ 50 ms (carry v2's bar).

## Durable docs migration on merge

Per `docs/plans/README.md` convention, when this work merges:

- `docs/design.md` grows the v3 Manager1 / Session1 surface + body-dict reference + inheritance subsection (per the commit 11 sketch above).
- This plan file is deleted; git history is the archive.
- v2's `docs/plans/profile-unification.md` was already deleted by v2's plan migration; no v2 plan artifacts remain in tree.
- The architectural lesson worth preserving: "configuration over D-Bus is property-bag-shaped, not typed-property-per-object — the BlueZ pattern works for operational state, not for configuration." This goes into `docs/design.md`'s introduction of the Manager1 surface as a one-paragraph rationale, citing the FOSS survey indirectly.

## Sequencing rationale

Phase 1 (scaffolding) lands the dep bump first because every other phase needs zvariant 5.11+.

Phase 2 (UI cherry-picks) lands second because the polish work is wire-independent. Visual review happens early; daemon + GUI rebind in phases 3–4 land against a known-good visual baseline.

Phase 3 (daemon) lands before phase 4 (GUI rebind) because GUI integration tests need the daemon surface to dispatch against. Commit 8 and commit 9 are paired by bus side but each self-contained; either could land first if a reviewer pipeline demanded it.

Phase 4 (GUI rebind) is the single biggest commit. Splitting it would create intermediate states where the pane is half-bound. The wire dispatch + the editor cache + the field-kind dispatch + the rename UX + the duplicate UX are all coupled: any subset of them in isolation produces a non-functional pane.

Phase 5 (docs migration) is the conventional plan-doc cleanup. Same shape as v2's commit 35.

## Cross-references

- v1 PR: <https://github.com/ruffsl/TisVlay/pull/58>
- v2 PR: <https://github.com/ruffsl/TisVlay/pull/59>
- zvariant 5.11.0 release (nested-dict `DeserializeDict`): <https://github.com/z-galaxy/zbus/releases/tag/zvariant-5.11.0>
- zbus issue #312 (the nested-dict gap): <https://github.com/z-galaxy/zbus/issues/312>
- zbus PR #1779 (the fix): <https://github.com/z-galaxy/zbus/pull/1779>
- FOSS prior-art survey synthesizing why property-bag is the idiomatic shape for typed configuration: searchable via git log on `docs/plans/profile-unification.md` (now deleted) under v2 commits `301ad5f` + `b43446d`.
