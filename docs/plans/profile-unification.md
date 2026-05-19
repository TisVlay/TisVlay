# Profile-CRUD v2 unification & field-kind extensibility

## Motivation

The just-shipped profile-CRUD v2 work lives across `crates/tisvlayd/src/dbus/system_profile.rs` (317 lines) and `crates/tisvlay-session/src/user_profile.rs` (1052 lines). The two interfaces are isomorphic — same per-profile object pattern, same typed properties, same `Properties.Set` + `ResetField` + `ResetAll` shape — but the source diverged. The system side delegates the heavy lifting (create / delete / duplicate / reset / refresh, name validation, persistence) to `TvManager`; the user side grew an inline parallel of that same machinery as free functions taking `&Arc<RwLock<SessionState>>`. Roughly **~700 lines of `user_profile.rs` are parallels of `TvManager` methods** rather than legitimate user-side specialization. The per-property getter/setter repetition (~80 lines per interface) is a real but secondary find.

In parallel, the field-editor row in the Profiles pane is enum-only — `ProfileFieldRow` renders a ComboBox over `SessionAction` values. `UserProfile1.IdleTimeoutSeconds` (typed `u`, range `0..=86400`) is therefore intentionally absent from `USER_FIELDS` in `crates/tisvlay-gui/src/profiles.rs`. Filed as a deferred idea ([docs/ideas/profile-user-idle-timeout-spinner.md](../ideas/profile-user-idle-timeout-spinner.md)) but the underlying problem — no row-type dispatch beyond enums — blocks every future non-enum field (`Number`, `Bool`, `Text`).

This plan does the structural cleanup and the field-kind unlock in one branch. Net savings: ~850–1000 lines deleted in daemon/session code, ~50 lines added in GUI for the field-kind dispatch.

No wire-protocol changes user-visible (per-property names, signatures, paths all stay the same). Internal Rust shape changes substantially.

## What's decided

Five refactors, landed in order. Each is a discrete commit (or small series).

### P1 — Hoist user-side profile-CRUD onto a `SessionProfileManager` (≈ −300 lines)

The free functions in `crates/tisvlay-session/src/user_profile.rs` (`create_user_profile`, `duplicate_user_profile`, `delete_user_profile`, `reset_user_profile`, `refresh_user_profile_cached_fields`, `user_profile_locally_set_fields`, `user_profile_referenced_by`, plus the inline `set_user_profile_*_field` helpers) move onto a `SessionProfileManager` struct owned by `SessionState`. Param signatures collapse from `(state: &Arc<RwLock<SessionState>>, config_path: Option<&PathBuf>, …)` to `&self` / `&mut self`. The `UserProfileEvent` channel stays where it is — the manager owns the sender and the forwarder task owns the receiver.

Pure refactor, no behavior change. Touches `crates/tisvlay-session/src/user_profile.rs`, `crates/tisvlay-session/src/lib.rs` (`SessionState` gets a `pub profile_manager: SessionProfileManager` field), `crates/tisvlay-session/src/session_dbus.rs` (Session1 method bodies call through the manager). Tests should pass unchanged since the surface they hit is the D-Bus interface, not the internal helpers.

### P2 — Typed field-kind registry + Slint dispatch (≈ +50 lines, unblocks numeric / bool / text fields)

One source of truth for field metadata, used by daemon-side property dispatch, session-side property dispatch, and GUI-side row rendering. Lives in `tisvlay-core` so all three sides import the same const tables.

```rust
// crates/tisvlay-core/src/profile_fields.rs (new)
pub enum FieldKind {
    Enum { options: &'static [&'static str] },
    Number { min: i64, max: i64, step: i64, unit: &'static str },
    Bool,
    Text { max_len: usize },
}

pub struct FieldDef {
    pub name: &'static str,           // wire id: "on_startup", "idle_timeout_seconds"
    pub label_msgid: &'static str,    // gettext msgid for the UI label
    pub kind: FieldKind,
}

pub const SYSTEM_FIELDS: &[FieldDef] = &[/* … */];
pub const USER_FIELDS: &[FieldDef] = &[
    FieldDef { name: "on_session_lock", label_msgid: "On session lock",
               kind: FieldKind::Enum { options: SESSION_ACTIONS } },
    /* … */
    FieldDef { name: "idle_timeout_seconds", label_msgid: "Idle timeout (seconds)",
               kind: FieldKind::Number { min: 0, max: 86400, step: 1, unit: "seconds" } },
];
```

GUI side:

```slint
// crates/tisvlay-gui/ui/globals/profiles.slint
export struct ProfileFieldEntry {
    field-name: string,
    label: string,
    kind: string,        // "enum", "number", "bool", "text"
    options: [string],   // populated for kind == "enum"
    current-value: string,
    number-value: int,   // populated for kind == "number"
    bool-value: bool,    // populated for kind == "bool"
    min-value: int,
    max-value: int,
    step: int,
    unit: string,
    is-locally-set: bool,
}
```

```slint
// crates/tisvlay-gui/ui/components/profiles_pane.slint
for entry[i] in Profiles.editor-fields:
    if entry.kind == "enum":   ProfileFieldRowEnum   { entry: entry; … }
    if entry.kind == "number": ProfileFieldRowNumber { entry: entry; … }
    if entry.kind == "bool":   ProfileFieldRowBool   { entry: entry; … }
    if entry.kind == "text":   ProfileFieldRowText   { entry: entry; … }
```

Each row component wraps the appropriate widget — Slint's built-in `SpinBox` (with `read-only`, `step-size`, `edited` callback) for `Number`, `Switch` for `Bool`, `LineEdit` for `Text`. The reset glyph + 2 px left gutter accent + `is-locally-set` semantics are shared via a common header strip; only the value-input control varies.

Rust side: `Profiles.value-edited` callback grows a kind-aware payload. Today's `(field_name: string, value: string)` becomes either a tagged enum payload or a per-kind callback set (`value-edited-enum(name, value)`, `value-edited-number(name, value)`, …). Decision deferred to implementation — both shapes work; pick whichever reads cleaner once the row components are in place.

Net cost: +50 lines for the second row variant (`ProfileFieldRowNumber`) plus the shared header strip. Subsequent row variants are ~15 lines each. `IdleTimeoutSeconds` joins `USER_FIELDS` once `Number` is in.

Field-kind metadata removes from `crates/tisvlay-gui/src/profiles.rs` — the GUI imports `SYSTEM_FIELDS` / `USER_FIELDS` from `tisvlay-core` and maps each `FieldDef` to a `ProfileFieldEntry` instead of carrying its own copy.

### P3 — Generic `ProfileService<K>` + `ProfileKind` trait (≈ −300 to −400 lines)

```rust
pub trait ProfileKind: 'static {
    type Config;                          // SystemProfileConfig | UserProfileConfig
    type Resolved;                        // ResolvedSystemProfile | ResolvedUserProfile
    type MutationError: Into<zbus::fdo::Error>;
    fn interface_name() -> &'static str;  // "org.tisvlay.SystemProfile1" | "…UserProfile1"
    fn object_path(uuid: &Uuid) -> String;
    fn fields() -> &'static [FieldDef];
    /* … per-field apply / clear / read accessors */
}

pub struct ProfileService<K: ProfileKind> {
    pub uuid: Uuid,
    pub name: String,
    pub fields: HashMap<&'static str, FieldValue>,  // typed union, dispatched by FieldKind
    pub locally_set_fields: Vec<String>,
    pub referenced_by: Vec<String>,
    _kind: PhantomData<K>,
}
```

zbus 5.x's `#[interface]` macro accepts generic `impl<K> ProfileService<K>` blocks but the interface name is monomorphic (resolved at macro-expansion time, not generic over `K`). Workaround: two newtype wrappers carry the interface name attribute:

```rust
pub struct SystemProfileService(ProfileService<SystemKind>);
pub struct UserProfileService(ProfileService<UserKind>);

#[interface(name = "org.tisvlay.SystemProfile1")]
impl SystemProfileService { /* methods delegate to self.0 */ }

#[interface(name = "org.tisvlay.UserProfile1")]
impl UserProfileService  { /* methods delegate to self.0 */ }
```

The bulk of the implementation — the typed property getters/setters, the `set_field` dispatch, the `ResetField` / `ResetAll` plumbing, the cache refresh — lives once on `ProfileService<K>`. The newtypes are thin delegation layers.

Risk: medium. Trait error messages get noisier; one more layer of indirection between the wire surface and the manager. Pays for itself because we go from "two parallel impls" to "one impl + cheap K substitution."

### P4 — Declarative macro for the residual per-field property pairs (≈ −150 lines)

Even with P3's generic, each `#[zbus(property)]` getter/setter pair has to be spelled out — zbus's macro doesn't auto-derive setters from struct field access. A workspace-local declarative macro expands the pairs from a field list:

```rust
profile_field_props! {
    on_startup: String,
    on_shutdown: String,
    /* … */
}
```

Expands to seven getter/setter pairs that all route through `self.set_field("on_startup", value).await`. Needs `paste` for identifier concatenation; `paste` is already a transitive dep of the zbus / slint stacks. No new top-level dep.

Lives in `tisvlay-core` (or in the consuming crate — declarative macros are per-crate-fine). Stays as `macro_rules!` unless a proc-macro need surfaces.

### P5 — Unified inheritance walker (≈ −80 lines)

`SystemSection::resolve_profile_by_uuid` walks 2 hops (named → default → baseline); `UserSection::resolve_profile_by_uuid` walks 4 hops (named → user-default → admin-by-name → admin-default → baseline). Both fold the chain the same way, just with different chain lengths and one user-specific name-keyed step.

```rust
fn resolve_chain<C: ProfileChainItem>(chain: impl IntoIterator<Item = Option<&C>>) -> Resolved<C> {
    /* fold last-wins-with-overlay across the chain */
}
```

Each `SystemSection` / `UserSection` builds its own iterator and hands it off. Pure refactor inside `crates/tisvlay-core/src/config.rs`. Very low risk.

## Sequencing rationale

P1 first because it's pure refactor with no behavior change — closes the asymmetry that makes the diff look huge, easiest to review, gives the cleanest "before / after" line-count delta. Single PR-worthy commit on its own merits.

P2 second because it ships the field-kind metadata shape that P3 needs to abstract over. Doing P3 before P2 would mean designing the trait against today's enum-only `String` properties, then either re-doing the trait when P2 lands or carrying a second-pass adjustment. P2 also delivers user-visible value (unblocks `IdleTimeoutSeconds` and any future numeric/bool field) which justifies its mid-branch landing if the branch slips and we ship without P3-P5.

P3 third — now informed by the typed-field shape — collapses the two parallel `#[interface]` impls into one generic with two newtype wrappers.

P4 fourth as a cleanup pass inside the generic, eliminating the residual per-property getter/setter boilerplate.

P5 last because it's small, isolated, and the lowest-risk win — perfect ending move that doesn't depend on anything else in the sequence.

## Dependency decisions

- **No new top-level deps required.** `paste` is already in the transitive tree (via zbus / slint). The field-kind work is pure Rust + Slint.
- **`tisvlay-macros` crate is not introduced now.** Proc macros (custom derive, attribute macros) require a separate crate type (`proc-macro = true` in `Cargo.toml`). Declarative `macro_rules!` macros do not. P4 is declarative. If a later refactor needs a proc macro (e.g. `#[derive(ProfileFields)]` that reads a struct definition), stand up `tisvlay-macros` at that point — it's a routine pattern (`serde` / `serde_derive`, `tokio` / `tokio-macros`, `zbus` / `zbus_macros`) and adding it later carries no migration burden.
- **`tisvlay-core` is the home for `FieldDef` / `FieldKind` / `SYSTEM_FIELDS` / `USER_FIELDS`.** Daemon, session, GUI all import from one place. Gettext `msgid` strings cross the core/GUI boundary but that's fine — `msgid` is a stable wire-format string, not a translated value.

## What's not in scope (real divergences that resist unification)

- **`TvManager` ownership vs `SessionState` ownership.** Daemon owns TVs + system profiles in one struct; session owns user profiles + a daemon client + Wayland listeners + idle watchers. Different lifetimes and concerns. P1 hoists user-side helpers onto a `SessionProfileManager` *within* `SessionState`; it does **not** try to fold that into a single cross-process struct.
- **State-change fan-out plumbing.** Daemon uses `TvManager`'s internal `StateChangeEvent` channel (private to the daemon process). Session uses an explicit `UserProfileEventTx` (private to the session process). Both work, both are correct. The asymmetry reflects the process boundary, not a missed abstraction.
- **Admin-baseline lookup in user inheritance.** Name-keyed (admin reuses baselines by profile name across users). No system-side equivalent. P5's unified walker takes the chain as input; the user-side iterator includes the admin step, the system-side iterator doesn't.
- **Disk persistence paths.** System config is `/var/lib/tisvlay/daemon.toml` (root-writable); user config is `~/.config/tisvlay/session.toml` (user-writable). Different paths, different writer identities. The unification stops at the in-memory shape.

## Risks

- **`#[interface]` macro generic limits.** zbus 5.x supports generic `impl<K>` blocks but the interface-name attribute is monomorphic. The newtype-wrapper pattern in P3 is the known-working escape hatch; if it surprises (zbus version bumps, generic-bound interactions), fall back to P3-without-newtypes — two `#[interface]` impls that each delegate to a shared `ProfileServiceInner` struct. Same line savings, marginally less elegant.
- **Trait error message readability.** `ProfileKind` adds a generic parameter to every error trace inside the service code. We accept this cost in exchange for the dedup.
- **Slint heterogeneous repeater rendering.** P2's `if entry.kind == "enum"` / `"number"` / … pattern is idiomatic Slint and used elsewhere in the codebase (the conditional reset-glyph render in today's `ProfileFieldRow`), but the four row components add component-instantiation overhead per row. For ~6–7 rows per profile detail, the overhead is irrelevant. If a future pane needs hundreds of variant rows, revisit.
- **Field-kind metadata desync.** `FieldDef` lives in `tisvlay-core`; the underlying struct fields live in `SystemProfileConfig` / `UserProfileConfig` (also `tisvlay-core`). Adding a field requires touching both. Mitigation: a `cargo test` that walks each `FieldDef` and asserts the named field exists on the corresponding config struct via a derive macro or an exhaustive match. Defer until a field-add actually causes a desync; the failure mode is loud (`set_field` panics on unknown name).

## Migration path on durable docs after this plan ships

Per `docs/plans/README.md` convention, when this work merges:

- The generic-`ProfileService<K>` shape and the `ProfileKind` trait fold into the "Per-profile interfaces" subsection of [docs/design.md](../design.md).
- The `FieldDef` / `FieldKind` registry pattern folds into a new "Field-kind registry" subsection alongside it — covers the contract for adding a new field type (config struct + `FieldDef` entry + a `ProfileFieldRow*` component if it's a new kind).
- The Slint heterogeneous-repeater rationale folds into [.claude/slint.md](../../.claude/slint.md) as a recipe for future variant-list panes.
- This plan file is deleted; git history is the archive.
