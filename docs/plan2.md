# Plan 2 — Detangle Test Harness From Published Crates

**Motivation:** We want the option to publish `tisvlay-core`, `tisvlay-lg`,
`tisvlay-backends`, and the binary crates to crates.io (at `0.y.z` — see
semver §4) while keeping `tisvlay-test-support`, `tisvlay-mock`, and the
per-daemon test harnesses as `publish = false` internal crates.

Cargo's crates.io rule ([Cargo Reference — Specifying
Dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html)):

> *crates.io does not allow packages to be published with dependencies on
> code published outside of crates.io itself (dev-dependencies are
> ignored).*

Today [tisvlayd](../crates/tisvlayd/Cargo.toml#L54) and
[tisvlay-session](../crates/tisvlay-session/Cargo.toml#L46) depend on
`tisvlay-test-support` as an **optional regular dependency** gated behind
the `mock` feature. "Optional" still means "listed in `[dependencies]`",
so crates.io would refuse to publish the daemon/session crates unless
`tisvlay-test-support` is also on the registry. Moving the harness code
out breaks that constraint cleanly.

Prerequisite for: Plan 1 Phase 2.5 Part B's `publish = false` plumbing,
and any future Plan 1 extension that actually ships to crates.io.

---

## Goal

After this plan:

1. `tisvlayd`, `tisvlay-session`, `tisvlay-cli`, `tisvlay-tray` have no
   regular (non-dev) dependency on any `publish = false` crate.
2. The `mock` feature on `tisvlayd`/`tisvlay-session` reduces to pure
   backend-registry passthrough (`tisvlay-backends/mock`) — no harness
   plumbing.
3. Integration tests still run identically via
   `cargo nextest run --workspace --all-features`.
4. `tisvlay-test-support`, `tisvlay-mock`, and the new harness crates stay
   workspace-private (`publish = false`). All others become publish-ready
   when we choose to flip the switch.

---

## Architecture — before vs after

### Before
```
tisvlayd (bin+lib)
├── [dependencies]
│   ├── tisvlay-backends
│   └── tisvlay-test-support  (optional, feature = "mock")  ← blocks publish
├── src/
│   └── test_harness.rs       (feature = "mock", uses crate::dbus, ::tv_manager)
└── tests/*.rs                (integration tests, use tisvlayd::test_harness)
```

### After
```
tisvlayd (bin+lib)
├── [dependencies]
│   └── tisvlay-backends
├── [dev-dependencies]
│   └── tisvlayd-harness (path, optional in dev only)
├── src/
│   └── lib.rs
│       └── #[doc(hidden)] pub mod __internals { pub use crate::...; }
└── tests/*.rs                (use tisvlayd_harness::TestDaemon)

tisvlayd-harness (new crate, publish = false)
├── [dependencies]
│   ├── tisvlayd                (path + version)
│   ├── tisvlay-test-support
│   └── tempfile
└── src/
    └── lib.rs                  (moved from tisvlayd/src/test_harness.rs)
```

Same pattern mirrored for `tisvlay-session` ↔ `tisvlay-session-harness`.

---

## Why `#[doc(hidden)] pub mod __internals`

`test_harness.rs` reaches into `crate::dbus::{build_dbus_connection,
BusKind, ManagerService}`,
[tv_manager::TvManager](../crates/tisvlayd/src/test_harness.rs#L44-L46),
`state_forwarder::state_change_forwarder`. Once the harness lives in a
separate crate, those items must be reachable externally.

Making every such item `pub` pollutes the stable public API of a
published crate. The `#[doc(hidden)] pub mod __internals { pub use ... }`
idiom (used by `serde`, `syn`, `tokio-util`, etc.) threads the needle:

- reachable from the harness crate by explicit path,
- absent from `cargo doc` output (`#[doc(hidden)]`),
- clearly signals "internal, no semver guarantee" to anyone who stumbles
  on it,
- is the Rust ecosystem's established convention.

---

## Phase plan

### Phase A — `tisvlayd-harness`

- [ ] Create `crates/tisvlayd-harness/` with `Cargo.toml`:
  - `name = "tisvlayd-harness"`
  - `publish = false`
  - `[dependencies]` = `tisvlayd` (path + version), `tisvlay-test-support`,
    `tempfile`, `tokio`, `zbus`, `tracing` as needed
- [ ] Copy [crates/tisvlayd/src/test_harness.rs](../crates/tisvlayd/src/test_harness.rs)
      content into `crates/tisvlayd-harness/src/lib.rs`; replace
      `use crate::...` with `use tisvlayd::__internals::...`.
- [ ] In [crates/tisvlayd/src/lib.rs](../crates/tisvlayd/src/lib.rs), add:
  ```rust
  #[doc(hidden)]
  pub mod __internals {
      pub use crate::dbus::{build_dbus_connection, BusKind, ManagerService};
      pub use crate::state_forwarder::state_change_forwarder;
      pub use crate::tv_manager::TvManager;
      // ...plus anything else test_harness needs
  }
  ```
- [ ] Bump visibility where required: items in `crate::dbus`,
      `crate::state_forwarder`, `crate::tv_manager` that are currently
      `pub(crate)` must become `pub` if re-exported.
- [ ] Delete `crates/tisvlayd/src/test_harness.rs` and its `pub mod
      test_harness;` line in `lib.rs`.
- [ ] Simplify the `mock` feature in `tisvlayd/Cargo.toml`:
  ```toml
  mock = ["tisvlay-backends/mock"]
  ```
  Remove the `dep:tempfile`, `dep:tisvlay-test-support` entries.
- [ ] Move `tempfile` and `tisvlay-test-support` out of `[dependencies]`
      in `tisvlayd/Cargo.toml`.
- [ ] Add `tisvlayd-harness` to the workspace `members` list in
      [Cargo.toml](../Cargo.toml).
- [ ] Update the 6 integration tests under `crates/tisvlayd/tests/*.rs`:
      replace `use tisvlayd::test_harness::TestDaemon` (etc.) with
      `use tisvlayd_harness::TestDaemon`. Add `tisvlayd-harness` to
      `[dev-dependencies]` in `tisvlayd/Cargo.toml`.
- [ ] Run `cargo nextest run -p tisvlayd --all-features` — green.

**Commit**: `refactor(tisvlayd): extract test_harness into tisvlayd-harness crate`

### Phase B — `tisvlay-session-harness`

Same shape as Phase A, applied to
[crates/tisvlay-session/src/test_harness.rs](../crates/tisvlay-session/src/test_harness.rs).

- [ ] Create `crates/tisvlay-session-harness/`.
- [ ] Move `test_harness.rs` → `tisvlay-session-harness/src/lib.rs`.
- [ ] Add `#[doc(hidden)] pub mod __internals` in
      `tisvlay-session/src/lib.rs` exporting
      `session_monitor::SessionEvent` and whatever else `TestSession`
      needs.
- [ ] Simplify `mock` feature, drop regular-dep plumbing, move the
      `dep:tisvlayd` transitive dep to the harness crate.
- [ ] Update `tisvlay-session/tests/session_events.rs` and
      `session_dbus_contract.rs`.
- [ ] Run `cargo nextest run -p tisvlay-session --all-features` — green.

**Commit**: `refactor(tisvlay-session): extract test_harness into tisvlay-session-harness crate`

### Phase C — Cross-crate test fixups

- [ ] [crates/tisvlay-cli/Cargo.toml](../crates/tisvlay-cli/Cargo.toml#L33)
      currently has `tisvlayd = { path = "../tisvlayd", features = ["mock"] }`
      as a dev-dep. After Phase A, `tisvlayd`'s `mock` feature no longer
      pulls the harness. Tests that used `test_harness` via this dev-dep
      should pull `tisvlayd-harness` directly instead.
- [ ] `crates/tisvlay-cli/tests/pair_snapshot.rs` — audit, update
      imports.
- [ ] Full workspace run: `just test` — green.

**Commit**: `refactor(tests): route harness access through dedicated crates`

### Phase D — Apply Plan 1 Phase 2.5 Part B (publish metadata)

Re-do the `publish = false` plumbing from Plan 1 Phase 2.5 Part B on
top of the new layout:

- [ ] Add `publish = false` to `[workspace.package]` in the root.
- [ ] Add `publish.workspace = true` to every crate `[package]`
      block — including `tisvlayd-harness`, `tisvlay-session-harness`,
      `tisvlay-test-support`, `tisvlay-mock`.
- [ ] Verify `cargo metadata --format-version 1 --no-deps` reports
      `"publish":[]` (cargo's representation of `publish = false`) for
      every workspace member.
- [ ] Add `[profile.release]` tuning (`lto = "thin"`, `codegen-units =
      1`, `strip = "symbols"`) per Plan 1 Phase 2.5 Part B.
- [ ] Plan 1 Phase 2.5 Part A: drop `cargoBuildFlags = [ "--all-features" ]`
      from [packaging/nix/package.nix](../packaging/nix/package.nix#L26)
      now that it's safe to do so (the production binary no longer pulls
      harness code even with `--all-features`, but matching the distro
      default features is still the right thing).

**Commit**: `cargo: publish metadata + release profile tuning`

### Phase E — Docs

- [ ] Update [.claude/CLAUDE.md](../.claude/CLAUDE.md) project-structure
      block:
  - list `tisvlayd-harness` and `tisvlay-session-harness` as new members
  - update the `tisvlayd/src/test_harness.rs` and
    `tisvlay-session/src/test_harness.rs` entries (remove)
  - note the `__internals` idiom briefly
- [ ] Update [docs/design.md](design.md) if it references the harness
      modules by file path.
- [ ] Update [docs/testing.md](testing.md) — mock backend and harness
      section.

**Commit**: `docs: describe harness crate layout`

---

## Out of scope for this plan

- Actually publishing anything to crates.io — that's a separate decision.
  This plan only makes publishing *possible* without dragging internal
  test crates along.
- Refactoring the `mock` backend itself — `tisvlay-mock` stays
  `publish = false` because its value as a standalone crate is marginal;
  if someone ever wants it, flip to `publish = true` at that time.
- Moving `tisvlay-test-support` — stays where it is, stays
  `publish = false`.

---

## Risk log

| Risk | Mitigation |
|---|---|
| Making `crate::dbus::*` etc. reachable via `__internals` could tempt third parties to use them | `#[doc(hidden)]` hides them from rendered docs; module name `__internals` signals "don't"; semver §4 (0.y.z) explicitly disclaims API stability anyway |
| Moving test_harness breaks unrelated callers we didn't find | Grep for `test_harness`, `TestDaemon`, `TestSession` (done — 15 files, 8 are tests, 2 are `mod` declarations, rest are documentation); `just test` catches the rest |
| Circular dependency (`tisvlayd-harness → tisvlayd → ?`) | `tisvlayd` does **not** depend on `tisvlayd-harness` in regular deps, only dev-deps — no cycle. Verified mentally; `cargo metadata` will confirm. |
| Visibility bumps from `pub(crate)` → `pub` leak internals | All leaked items go through `__internals` — not in the main crate namespace — and `#[doc(hidden)]` keeps them out of public API docs |

---

## Effort estimate

| Phase | Scope | Est |
|---|---|---|
| A | tisvlayd harness extract | 2–3 h |
| B | tisvlay-session harness extract | 1–2 h |
| C | Cross-crate test fixups | 30 min |
| D | publish metadata + profile | 20 min (mostly reapplying reverted changes) |
| E | Docs | 30 min |
| **Total** | | **~4–6 h of focused work**, one PR |

---

## Go / no-go decision criteria

Do this if: we might publish tisvlay-core or binaries to crates.io within
the next ~12 months, **or** if we want the cleaner
public/internal separation regardless of publishing timeline.

Skip if: we're committed to distro-only distribution indefinitely and
the current test harness layout isn't causing friction. In that case
Plan 1 Phase 2.5 Part B (just `publish = false` everywhere) is enough.
