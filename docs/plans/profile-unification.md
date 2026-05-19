# Profile-CRUD v2 unification & field-kind extensibility

## Motivation

The just-shipped profile-CRUD v2 work has two duplicated implementations of the same essential pattern: `crates/tisvlayd/src/dbus/system_profile.rs` (317 lines) and `crates/tisvlay-session/src/user_profile.rs` (1052 lines). The interfaces are isomorphic — same per-profile object pattern, same typed properties + `Properties.Set` + `ResetField` / `ResetAll`, same `LocallySetFields` + `ReferencedBy` companion properties — but the source diverged.

A previous attempt at this work landed two commits (`SessionProfileManager` extraction; field-kind registry + spinner) but inflated the branch by ~500 lines while claiming dedup. The branch was hard-reset back to the pre-refactor baseline; this plan is the honest re-scope after a FOSS prior-art survey.

In parallel, the field-editor row in the Profiles pane is enum-only — `ProfileFieldRow` renders a ComboBox over `SessionAction` values. `UserProfile1.IdleTimeoutSeconds` (typed `u`, range `0..=86400`) is therefore intentionally absent from the GUI's `USER_FIELDS` table. The underlying problem — no row-type dispatch beyond enums — blocks every future non-enum field (`Number`, `Bool`, `Text`).

## What the FOSS survey showed

Five FOSS daemons surveyed: NetworkManager, systemd-homed, BlueZ, AccountsService, PowerProfiles, plus context from logind, GeoClue, PowerDevil / KConfigXT.

Findings that changed the scope:

1. **Our wire shape is approximately idiomatic.** Per-object pattern, typed `#[zbus(property)]` properties, `Properties.Set` mutation — all match BlueZ (the cleanest precedent). No grand restructure is warranted.
2. **Wire-level unification of the two interfaces** (one `Profile1` schema served on both buses) is not viable. The field sets are disjoint (system: `OnStartup` / `OnShutdown` / …; user: `OnSessionLock` / `OnIdleTimeout` / …), so a union schema would expose dead properties per bus. An `a{sv}` body bag (NetworkManager-style) loses per-field `PropertiesChanged` signals our GUI relies on.
3. **Rust-impl unification via generics has language constraints** — zbus's `#[interface(name=...)]` is monomorphic per impl block; struct fields differ per kind. The previous attempt at a generic `ProfileService<K>` newtype-wrapper hit these constraints and saved fewer lines than it added in trait machinery.
4. **The actual duplication is per-property setter boilerplate** — the same 4-line shape repeated 7 times on the system side and 6 times on the user side. A declarative macro collapses this cleanly, with no new abstraction layer.

## Scope

Three deliverables planned; **deliverable 3 was dropped during execution** after discovering a zbus macro-expansion limitation (see "What was attempted but dropped" below). The shipped set is the first two.

### 1. `trait ProfileManager` + `SessionProfileManager` extraction

`tisvlay-core` gains `trait ProfileManager` with four async methods covering the per-profile mutation surface:

```rust
pub trait ProfileManager: Send + Sync + 'static {
    async fn set_string_field(&self, uuid: Uuid, field: &str, value: &str) -> Result<(), ProfileError>;
    async fn set_u32_field(&self, uuid: Uuid, field: &str, value: u32) -> Result<(), ProfileError>;
    async fn reset_field(&self, uuid: Uuid, field: &str) -> Result<(), ProfileError>;
    async fn reset_all(&self, uuid: Uuid) -> Result<(), ProfileError>;
}
```

`tisvlayd`'s `TvManager` implements `ProfileManager` for system profiles (delegates to existing `set_system_profile_field`, etc.). The system side has no numeric fields today; `set_u32_field` returns `ProfileError::UnknownField` for any caller, leaving room for future numeric system fields without further trait churn.

`tisvlay-session` gets a new `SessionProfileManager` struct that consolidates the inline `create_user_profile` / `duplicate_user_profile` / `delete_user_profile` / `reset_user_profile` / `refresh_*` free functions onto methods, and implements `ProfileManager`. State (`Arc<RwLock<SessionState>>`) + config path + event sender are bundled into the manager so the per-profile interface struct (`UserProfileService`) holds only `Arc<SessionProfileManager>` + cached fields, not three plumbing fields.

This piece is **scaffolding for the macro, not a standalone line-savings refactor**. The honest cost is roughly neutral on raw lines once you count the trait declaration overhead. The structural value is symmetry between the daemon-side and session-side mutation entry points + a uniform call shape the macro can target.

### 2. Field-kind registry + IdleTimeoutSeconds spinner

`tisvlay-core::profile_fields` — canonical `FieldDef` / `FieldKind` const tables.

```rust
pub enum FieldKind {
    Enum { options: &'static [&'static str] },
    Number { min: i64, max: i64, step: i64, unit: &'static str },
    Bool,
    Text { max_len: usize },
}

pub struct FieldDef {
    pub prop_name: &'static str,   // "OnStartup" — D-Bus property
    pub wire_name: &'static str,   // "on_startup" — LocallySetFields token + ResetField arg
    pub label_msgid: &'static str, // gettext msgid for the GUI label
    pub kind: FieldKind,
}

pub const SYSTEM_FIELDS: &[FieldDef] = &[/* 7 enum entries */];
pub const USER_FIELDS: &[FieldDef] = &[
    /* 5 enum entries */,
    FieldDef {
        prop_name: "IdleTimeoutSeconds",
        wire_name: "idle_timeout_seconds",
        label_msgid: "Idle timeout (seconds)",
        kind: FieldKind::Number { min: 0, max: 86_400, step: 1, unit: "seconds" },
    },
];
```

Only `Enum` and `Number` have GUI row components today; `Bool` and `Text` are placeholders in the kind enum so adding the first instance is a one-table-entry change.

GUI changes:

- `crates/tisvlay-gui/src/profiles.rs` deletes its local `FieldDef` + tables, imports from core.
- `ProfileFieldEntry` Slint struct grows `kind` + variant-specific fields (`number-value`, `min-value`, `max-value`, `step`, `unit`).
- New `ProfileFieldRowNumber` Slint component (wraps `std-widgets.slint::SpinBox`).
- New `Profiles.value-edited-number(string, int)` callback + Rust dispatch.
- Repeater in `profiles_pane.slint` dispatches on `entry.kind` via conditional `if entry.kind == "enum" / "number"`.

Closes `docs/ideas/profile-user-idle-timeout-spinner.md` (delete the file + its `docs/ideas.md` index entry).

### ~~3. `profile_field_props!` declarative macro~~ (dropped)

Was attempted. See "What was attempted but dropped" below.

## What was attempted but dropped

A declarative macro `profile_field_props!(on_startup, on_shutdown, ...)` to collapse the N typed-property getter/setter pairs into a single invocation per interface. The macro generated `#[zbus(property)] fn <field>(&self) -> &str` getters and `#[zbus(property)] async fn set_<field>(&mut self, value: String)` setters via `paste::paste!` for identifier composition.

**Why it didn't work.** zbus's `#[interface]` attribute macro parses the impl block with `syn` and iterates `ImplItem::Fn` + `ImplItem::Verbatim`. It does **not** expand inner `ImplItem::Macro` invocations — declarative macros inside the impl block are seen as opaque tokens. The compiler expands them *after* `#[interface]` runs, so the resulting `#[zbus(property)]` annotations on the macro-generated methods never make it through zbus's introspection generation. The runtime result: methods exist on the struct but the D-Bus property registration + `<field>_changed()` emitters are missing, and `Properties.Set` from a client misses the routing entirely.

Confirmed against `libs/zbus/zbus_macros/src/iface.rs:346` — only `ImplItem::Fn` and `ImplItem::Verbatim` arms in the dispatch loop.

**Workaround paths considered, all rejected for this branch.** (a) Write a custom proc-macro crate (`tisvlay-macros`) that wraps `#[interface]` and inserts the property pairs before forwarding to zbus's macro — high effort for the LoC savings on offer. (b) Wrap the entire `#[interface] impl { ... }` block in an outer declarative macro that emits the pairs inline — `paste!` for `set_<field>` is still inner-macro to `#[interface]`, same dead-end. (c) Build-script codegen of `.rs` files — adds a compile-time tool dependency.

If a future profile family adds enough fields to make the boilerplate painful again, the proc-macro crate is the right move. Filed mentally; not in scope here.

## What's NOT in scope

- **Generic `ProfileService<K>` newtype-wrapper pattern.** Original plan's "P3". Constrained by zbus macro monomorphism + per-kind struct fields. The macro takes most of what the generic was supposed to deliver without the abstraction tax.
- **Unified inheritance walker.** Original plan's "P5". Small standalone win; defer until a third profile family or a future inheritance-feature ask makes it worth touching.
- **Wire-level interface unification.** Architecturally inadvisable per the FOSS survey (disjoint field sets + per-field signal granularity).

## File-by-file changes

```
crates/tisvlay-core/
  src/lib.rs                            module decls + re-exports
  src/profile_fields.rs                 new — FieldDef + tables
  src/profile_manager.rs                new — trait ProfileManager + ProfileError

crates/tisvlayd/
  src/dbus/system_profile.rs            route through ProfileManager trait
  src/tv_manager.rs                     impl ProfileManager for TvManager

crates/tisvlay-session/
  Cargo.toml                            +async-trait dep
  src/user_profile.rs                   SessionProfileManager extraction
                                        + impl ProfileManager
                                        + UserProfileService rewires
  src/session_dbus.rs                   thread Arc<SessionProfileManager>
  src/main.rs                           retry task uses bundle.profile_manager
  src/lib.rs                            __internals re-export

crates/tisvlay-gui/
  src/profiles.rs                       delete local FieldDef + tables;
                                        kind-dispatched build_row;
                                        u32 read/write paths;
                                        dispatch_value_edit_number
  src/lib.rs                            on_value_edited_number callback
  ui/globals/profiles.slint             extended ProfileFieldEntry +
                                        value-edited-number
  ui/components/profiles_pane.slint     ProfileFieldRowEnum (renamed) +
                                        ProfileFieldRowNumber (new) +
                                        kind-dispatch in repeater

docs/
  ideas.md                              drop spinner entry
  ideas/profile-user-idle-timeout-spinner.md  delete (feature shipped)
  plans/profile-unification.md          this file (updated mid-execution)
```

## Expected outcome

The value here is structural + a user-visible feature, not raw line-count savings. Specifically:

- **Symmetric manager surface** between daemon and session sides via `trait ProfileManager`.
- **IdleTimeoutSeconds spinner ships** as a typed `Number` row in the Profiles pane.
- **Single source of truth** for field metadata in `tisvlay-core::profile_fields` — daemon, session, and GUI all import the same const tables.
- ~~Per-property setter boilerplate collapse via macro~~ — attempted, dropped (see "What was attempted but dropped"). The per-property pairs remain hand-written on both interfaces.

### Actual `tokei` delta (touched files)

| Language | Pre-refactor (code LoC) | Post-refactor | Δ |
|---|---|---|---|
| Rust | 9 360 | 9 543 | **+183** |
| Slint | 4 117 | 4 201 | **+84** |
| **Total** | **13 477** | **13 744** | **+267** |

Decomposition of the +267:

- Spinner feature: ~+85 Slint (new `ProfileFieldRowNumber` component) + ~+60 Rust (numeric proxy + dispatch paths).
- `ProfileManager` trait + `ProfileError` + `#[async_trait]` impls: ~+50 Rust.
- `profile_fields` module: +~230 Rust new, offset by deleting ~75 Rust of the GUI's local tables = **~+155 net**.
- `SessionProfileManager` extraction: ~neutral (manager methods replace free functions of similar length).

The dropped macro deliverable was the only piece of the plan that would have produced negative LoC; without it, **the branch is +267 LoC total** for the structural cleanup + spinner feature. Honest measurement.

## Risks

- **Macro hygiene.** Declarative macros with `paste::paste!` ident composition can be brittle when expanding inside `#[interface]` impl blocks (the macro must expand to valid `#[zbus(property)]` annotated methods). Mitigation: write the macro against a small fixture first, verify it works via introspection, then apply to the real interfaces.
- **Trait async signatures.** `trait ProfileManager` uses stable async-fn-in-trait. No `dyn ProfileManager` support means everything stays generic — fine for our call sites (which are monomorphic per profile kind).
- **Re-entrance.** `SessionProfileManager` methods take `&self` and lock state internally. Callers must not hold the state lock across manager calls (tokio RwLock isn't re-entrant). Same constraint the existing free functions follow.

## Verification

- `nix develop --command just check` end-to-end green.
- `nix develop --command prek run -a` end-to-end green.
- Full test suite (435 tests + doc tests) passes unchanged.
- Manual GUI smoke test: launch `tisvlay-gui`, verify (a) existing profile editor still works for the enum fields, (b) `IdleTimeoutSeconds` SpinBox renders and commits.
- `tokei` before / after comparison reported in the final commit body.

## Migration path on durable docs after this plan ships

Per `docs/plans/README.md` convention, when this work merges:

- `trait ProfileManager` + the macro pattern fold into [docs/design.md](../design.md) as a "Profile-interface abstraction" subsection.
- The `FieldDef` / `FieldKind` registry contract folds into a "Field-kind registry" subsection alongside it.
- The Slint heterogeneous-repeater rationale folds into [.claude/slint.md](../../.claude/slint.md) as a recipe for future variant-list panes.
- This plan file is deleted; git history is the archive.
