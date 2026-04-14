# Plan 2 — Detangle Test Harness From Published Crates

**Motivation:** We want the option to publish `tisvlay-core`, `tisvlay-lg`,
`tisvlay-backends`, and the binary crates to crates.io (at `0.y.z` — see
[semver §4](https://semver.org/#spec-item-4)) while keeping the test
infrastructure workspace-private.

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
out of the binary crates breaks that constraint cleanly.

Prerequisite for: any crates.io publishing work, and the
`publish = false` plumbing that was paused from Plan 1 Phase 2.5.

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
4. Test infrastructure (`tisvlay-test`, `tisvlay-mock`, `tisvlay-harness`)
   stays workspace-private (`publish = false`). All others become
   publish-ready when we flip the switch.

---

## Target crate layout

Four test-related crates, each with a crisp role:

| Crate | Role | Depends on | `publish` |
|---|---|---|---|
| **`tisvlay-test`** | Fakes the **OS environment** tisvlay talks to: `PrivateBus` (per-test `dbus-daemon`), `FakeLogind` (`PrepareForSleep`), `FakeScreenSaver` (`ActiveChanged`). Low-level primitives. | `zbus`, `tokio`, `tempfile` — nothing from the workspace | `false` |
| **`tisvlay-mock`** | Fakes a **TV on the other side of the protocol** — in-memory impl of the `tisvlay-core` traits (`TvBackend`, `TvConnection`, `TvConnector`, `TvDiscovery`). | `tisvlay-core` | `false` |
| **`tisvlay-harness`** | Combined **binary-lifecycle orchestrator**: `TestDaemon`, `TestSession`, `TestTray` in sibling modules with shared setup. Reaches into `tisvlayd`, `tisvlay-session`, `tisvlay-tray` internals via their `__internals` modules. | `tisvlayd`, `tisvlay-session`, `tisvlay-tray`, `tisvlay-test`, `tisvlay-mock` | `false` |
| *(dropped)* `tisvlay-test-support` | Renamed → `tisvlay-test` | — | — |

### Three-layer mental model

```
                      ┌──────────────────────────┐
                      │     tisvlay-harness      │  ← orchestrates binaries
                      │  daemon / session / tray │
                      └──────────────────────────┘
                            │         │
               boots        │         │   needs shared
               binary       ▼         ▼   primitives
                 ┌──────────────┐   ┌─────────────┐
(peer side)      │ tisvlay-mock │   │ tisvlay-test│  (env side)
                 │  (fake TV)   │   │ (fake bus,  │
                 │              │   │  logind,    │
                 │              │   │  screensvr) │
                 └──────────────┘   └─────────────┘
```

### Why these boundaries (and not more/fewer)

- **Split along publish boundaries** — production crates are `publish =
  true`, test crates are `publish = false`. That's the hard constraint.
- **Split along consumer type** — `tisvlay-mock` is reusable by any
  caller (core unit tests, CLI tests, future downstream-consumer tests)
  that needs a fake backend but not a full daemon. Keeping it distinct
  from `tisvlay-harness` preserves that.
- **Fold, don't fan out** — `TestDaemon`/`TestSession`/`TestTray` share
  significant setup (`PrivateBus` lifecycle, tracing init, mock backend
  registration). Three separate `*-harness` crates would duplicate or
  require a helpers crate beneath them. One `tisvlay-harness` with
  sibling modules is idiomatic (c.f. `tokio-test` exposing
  `block_on`/`io`/`task`).
- **Dev-deps do the gating** — no need for features on the harness
  crate; integration tests pull it via `[dev-dependencies]` only.

### Name rationale

| Crate | Convention match |
|---|---|
| `tisvlay-test` | `tokio-test`, `tower-test`, `serde_test`, `axum-test` — `<project>-test` is the established idiom for test utility crates |
| `tisvlay-mock` | `mockito`, `mockall`, `-mock` suffix is colloquial Rust. Technically a "fake" (functional in-memory), not a "mock" (records/asserts), but the colloquial usage is entrenched. |
| `tisvlay-harness` | `-harness` is less common but unambiguous — "spins up a full process with its dependencies for tests." Distinct from `tisvlay-test`'s lower-level primitives. |

---

## Before vs After

### Before
```
tisvlayd (bin+lib)
├── [dependencies]
│   ├── tisvlay-backends
│   └── tisvlay-test-support  (optional, feature = "mock")  ← blocks publish
├── [features]
│   └── mock = [..., "dep:tisvlay-test-support", "dep:tempfile"]
├── src/
│   └── test_harness.rs       (feature = "mock", uses crate::dbus, ::tv_manager)
└── tests/*.rs                (use tisvlayd::test_harness::TestDaemon)

tisvlay-session (similar shape)
tisvlay-tray    (no harness today)
```

### After
```
tisvlayd, tisvlay-session, tisvlay-tray (bin+lib)
├── [dependencies]
│   └── tisvlay-backends      (only)
├── [dev-dependencies]
│   └── tisvlay-harness       (path)
├── [features]
│   └── mock = ["tisvlay-backends/mock"]   ← just backend passthrough
└── src/lib.rs
    └── #[doc(hidden)] pub mod __internals {
            pub use crate::...;            // items the harness reaches into
        }

tisvlay-harness (new, publish = false)
├── [dependencies]
│   ├── tisvlayd              (path + version)
│   ├── tisvlay-session       (path + version)
│   ├── tisvlay-tray          (path + version)
│   ├── tisvlay-test
│   ├── tisvlay-mock
│   └── tempfile
└── src/
    ├── lib.rs                pub mod daemon;
    │                         pub mod session;
    │                         pub mod tray;
    ├── common.rs             shared PrivateBus/tracing/setup
    ├── daemon.rs             TestDaemon (moved from tisvlayd/src/test_harness.rs)
    ├── session.rs            TestSession (moved from tisvlay-session/src/test_harness.rs)
    └── tray.rs               TestTray (empty/skeleton for now)

tisvlay-test (renamed from tisvlay-test-support, publish = false)
tisvlay-mock (unchanged, publish = false)
```

---

## Why `#[doc(hidden)] pub mod __internals`

`test_harness.rs` reaches into private modules of its parent
([tisvlayd/src/test_harness.rs:44-46](../crates/tisvlayd/src/test_harness.rs#L44-L46)
imports `crate::dbus::{build_dbus_connection, BusKind, ManagerService}`,
`crate::state_forwarder::state_change_forwarder`, `crate::tv_manager::TvManager`;
[tisvlay-session's](../crates/tisvlay-session/src/test_harness.rs#L31-L32) uses
`crate::session_monitor::SessionEvent`, `crate::SessionState`).

Once the harness moves to a sibling crate, those items must be reachable
externally. Making each `pub` pollutes the stable API surface of
published crates.

The `#[doc(hidden)] pub mod __internals { pub use ... }` idiom threads
the needle:

- Reachable from the harness crate by explicit path.
- Absent from `cargo doc` output (`#[doc(hidden)]`).
- Module name `__internals` signals "not for public consumption."
- Established Rust convention (`serde::__private`, `syn::export`,
  `tokio-util::__private`).
- `0.y.z` semver disclaimer makes the "no stability promise" explicit
  regardless.

---

## Phase plan

### Phase A — Rename `tisvlay-test-support` → `tisvlay-test`

Small, mechanical, reversible. Lands first to minimize later churn.

- [ ] `git mv crates/tisvlay-test-support crates/tisvlay-test`.
- [ ] Update `name = "tisvlay-test-support"` → `"tisvlay-test"` in the
      crate's `Cargo.toml`; update `description`.
- [ ] Update the workspace `members` list in [Cargo.toml](../Cargo.toml#L7).
- [ ] Update `[workspace.dependencies]`:
      `tisvlay-test-support = { path = "crates/tisvlay-test-support" }`
      → `tisvlay-test = { path = "crates/tisvlay-test" }`.
- [ ] Update every crate's `[dependencies]`/`[dev-dependencies]` that
      references `tisvlay-test-support` (5 crates:
      `tisvlay-core`, `tisvlay-cli`, `tisvlay-session`, `tisvlayd`,
      `tisvlay-mock`? — verify via grep).
- [ ] Update imports: `use tisvlay_test_support::...` →
      `use tisvlay_test::...` across the tree.
- [ ] Update [.claude/CLAUDE.md](../.claude/CLAUDE.md) project-structure
      block and [docs/testing.md](testing.md).
- [ ] `just test` — green.

**Commit**: `refactor(test-support): rename crate to tisvlay-test (matches tokio-test convention)`

### Phase B — Create `tisvlay-harness` crate skeleton

Introduce the new crate before moving code, so each subsequent move is
small and independently verifiable.

- [ ] Create `crates/tisvlay-harness/` with:
  - `Cargo.toml`: `publish = false`, deps on `tisvlay-test`,
    `tisvlay-mock`, `tokio`, `zbus`, `tracing`, `tempfile`. Binary-crate
    deps (`tisvlayd`, `tisvlay-session`, `tisvlay-tray`) added in
    subsequent phases as their `__internals` modules land.
  - `src/lib.rs`: `pub mod common; pub mod daemon; pub mod session; pub mod tray;`
    with empty module files to start.
  - `src/common.rs`: seed with placeholder comment; shared helpers land
    in Phase C as they're factored out.
- [ ] Add `tisvlay-harness` to workspace `members` and
      `[workspace.dependencies]`.
- [ ] `cargo check -p tisvlay-harness` — compiles (empty crate).

**Commit**: `refactor: add empty tisvlay-harness crate skeleton`

### Phase C — Migrate `TestDaemon` → `tisvlay-harness::daemon`

- [ ] In [tisvlayd/src/lib.rs](../crates/tisvlayd/src/lib.rs), add:
  ```rust
  #[doc(hidden)]
  pub mod __internals {
      pub use crate::dbus::{build_dbus_connection, BusKind, ManagerService};
      pub use crate::state_forwarder::state_change_forwarder;
      pub use crate::tv_manager::TvManager;
      // ...audit test_harness.rs imports, add anything missing
  }
  ```
- [ ] Bump `pub(crate)` → `pub` on items that need to travel through
      `__internals` (audit each; prefer re-exporting only what's
      necessary).
- [ ] Move `crates/tisvlayd/src/test_harness.rs` →
      `crates/tisvlay-harness/src/daemon.rs`; replace `use crate::...`
      with `use tisvlayd::__internals::...`.
- [ ] Delete `pub mod test_harness;` from `tisvlayd/src/lib.rs` and the
      file itself.
- [ ] In `tisvlay-harness/Cargo.toml`, add
      `tisvlayd = { workspace = true }` (path + version) to
      `[dependencies]`.
- [ ] Simplify `tisvlayd/Cargo.toml`'s `mock` feature to
      `mock = ["tisvlay-backends/mock"]`; drop
      `dep:tempfile`, `dep:tisvlay-test-support` entries and the
      corresponding `[dependencies]` lines.
- [ ] Factor shared setup (private bus init, tracing, mock-backend
      registration) into `tisvlay-harness/src/common.rs` now that
      there's a concrete first user.
- [ ] Update `crates/tisvlayd/tests/*.rs` (6 files): replace
      `use tisvlayd::test_harness::TestDaemon` with
      `use tisvlay_harness::daemon::TestDaemon`. Add `tisvlay-harness`
      to `tisvlayd`'s `[dev-dependencies]`.
- [ ] `cargo nextest run -p tisvlayd --all-features` — green.

**Commit**: `refactor(tisvlayd): move test_harness into tisvlay-harness::daemon`

### Phase D — Migrate `TestSession` → `tisvlay-harness::session`

Same shape as Phase C, applied to tisvlay-session.

- [ ] Add `#[doc(hidden)] pub mod __internals` to
      `tisvlay-session/src/lib.rs` exporting `session_monitor::SessionEvent`,
      `SessionState`, etc.
- [ ] Move `crates/tisvlay-session/src/test_harness.rs` →
      `crates/tisvlay-harness/src/session.rs`; rewire imports.
- [ ] Reuse `tisvlay-harness/src/common.rs` setup (this is where the
      fold-into-one-crate decision pays off).
- [ ] Simplify `tisvlay-session`'s `mock` feature. Drop `dep:tisvlayd`
      and `dep:tisvlay-test-support` from regular deps; if tests need a
      daemon running, `TestSession::new()` can compose
      `tisvlay-harness::daemon::TestDaemon` internally.
- [ ] Update `crates/tisvlay-session/tests/*.rs` (2 files).
- [ ] `cargo nextest run -p tisvlay-session --all-features` — green.

**Commit**: `refactor(tisvlay-session): move test_harness into tisvlay-harness::session`

### Phase E — Placeholder `TestTray` skeleton

Keeps the layout complete even before tray tests exist, so Phase F
(docs/cleanup) has no special case.

- [ ] Leave `tisvlay-harness/src/tray.rs` with a skeleton
      `TestTray` struct + constructor + `TODO` comment referencing the
      GUI/tray test plan. No actual harness logic yet — lands when tray
      integration tests are needed.
- [ ] Add `tisvlay-tray = { workspace = true }` to
      `tisvlay-harness/Cargo.toml` (optional — may be deferred until
      `TestTray` actually does something).

**Commit**: `refactor: scaffold tisvlay-harness::tray for future tray tests`

### Phase F — Cross-crate test fixups + Plan 1 Phase 2.5 replays

- [ ] [crates/tisvlay-cli/Cargo.toml](../crates/tisvlay-cli/Cargo.toml#L33)
      currently has `tisvlayd = { path = "../tisvlayd", features = ["mock"] }`
      as a dev-dep. After Phase C, `mock` no longer pulls harness code.
      Route CLI tests that use the daemon fixture through
      `tisvlay-harness` instead.
- [ ] [crates/tisvlay-cli/tests/pair_snapshot.rs](../crates/tisvlay-cli/tests/pair_snapshot.rs) —
      audit and update imports.
- [ ] Re-apply Plan 1 Phase 2.5 Part B (paused earlier):
  - Add `publish = false` to `[workspace.package]`.
  - Add `publish.workspace = true` to every crate's `[package]` block.
  - Verify via `cargo metadata --format-version 1 --no-deps` that all
    workspace members report `"publish":[]`.
  - Add `[profile.release]` tuning (`lto = "thin"`, `codegen-units =
    1`, `strip = "symbols"`).
- [ ] Re-apply Plan 1 Phase 2.5 Part A: drop
      `cargoBuildFlags = [ "--all-features" ]` from
      [packaging/nix/package.nix](../packaging/nix/package.nix#L26).
- [ ] Full workspace run: `just test` — green.
- [ ] `cargo package --workspace --dry-run -p tisvlay-core -p tisvlay-lg
      -p tisvlay-backends -p tisvlayd -p tisvlay-session -p tisvlay-cli
      -p tisvlay-tray` — no errors. (Smoke-tests that publishing works
      end-to-end; won't actually upload.)

**Commit**: `cargo: publish metadata + release profile + harness-aware test deps`

### Phase G — Docs

- [ ] Update [.claude/CLAUDE.md](../.claude/CLAUDE.md) project-structure
      block:
  - rename `tisvlay-test-support` → `tisvlay-test` (Phase A already
    edited, but verify the prose)
  - add `tisvlay-harness` section
  - drop the `test_harness.rs` entries from `tisvlayd` and
    `tisvlay-session` module lists
  - briefly describe the `__internals` idiom
- [ ] Update [docs/design.md](design.md) — any references to
      harness modules by file path.
- [ ] Update [docs/testing.md](testing.md) — mock backend and harness
      section; spell out the three-layer model.

**Commit**: `docs: describe new test crate layout`

---

## Out of scope for this plan

- Actually publishing anything to crates.io. This plan makes publishing
  *possible* without dragging test infrastructure along; the go/no-go
  on publishing is a separate decision.
- Refactoring the `mock` backend API itself. `tisvlay-mock` stays
  `publish = false` because its standalone value is marginal; if someone
  ever wants it, flip the flag.
- Tray integration tests themselves. Phase E only creates the
  placeholder so the layout is consistent; real `TestTray` logic lands
  when tray tests are authored.

---

## Risk log

| Risk | Mitigation |
|---|---|
| `__internals` re-exports tempt external use despite `#[doc(hidden)]` | Module name + doc-hidden + `0.y.z` disclaimer. Ecosystem precedent (serde, syn) shows it holds up. |
| Moving test_harness breaks unrelated callers we missed | Grep for `test_harness`, `TestDaemon`, `TestSession` first (16 files found in prior audit). `just test` catches the rest. |
| Circular dep (`tisvlay-harness → tisvlayd → ?`) | `tisvlayd` depends on `tisvlay-harness` only in `[dev-dependencies]` — no cycle. `cargo metadata` will confirm. |
| Visibility bumps from `pub(crate)` → `pub` leak internals to external users | All bumps go through `__internals` submodule, not the main namespace. `#[doc(hidden)]` keeps docs clean. |
| One big `tisvlay-harness` crate pulls all three binaries into every test's compile graph | Acceptable — all three are small, cargo only builds targets that tests need, can split later if it measurably hurts |
| Rename (`tisvlay-test-support` → `tisvlay-test`) touches many files | Phase A isolates it as its own commit. Mechanical `git mv` + sed. |

---

## Effort estimate

| Phase | Scope | Est |
|---|---|---|
| A | Rename `-test-support` → `-test` | 20–30 min |
| B | Empty `tisvlay-harness` crate skeleton | 15 min |
| C | Move `TestDaemon` + factor `common.rs` | 2–3 h (the bulk of the risk/work) |
| D | Move `TestSession` (mostly mechanical after C) | 45 min |
| E | `TestTray` placeholder | 15 min |
| F | Cross-crate fixups + replay Plan 1 Phase 2.5 | 30 min |
| G | Docs | 30 min |
| **Total** | | **~5–6 h focused work**, landable as one PR or split commit-by-commit |

---

## Go / no-go decision criteria

Do this if: we might publish any crate to crates.io within ~12 months,
**or** we want the cleaner public/internal separation regardless of
publishing timeline (useful even for documentation, code archaeology,
and onboarding).

Skip if: distro-only distribution is the permanent plan and the current
test harness layout isn't causing friction. In that case, the simpler
Plan 1 Phase 2.5 Part B (`publish = false` everywhere) is enough.
