# Review: Native journald logging via tracing-journald (2f3edb8..a4e2d5f)

Review of 4 commits (including a revert-and-redo) adopting `tracing-journald` for
structured native journal logging, replacing the previous `JOURNAL_STREAM` env var
detection with TTY-based gating.

Build: clean. Tests: all pass.

---

## Commit History Note

The sequence is: TTY detection attempt (2f3edb8) -> revert (4aa32c7) ->
tracing-journald adoption (2f2f91b) -> TTY guard fix (a4e2d5f). The revert was
necessary because the first attempt only changed timestamp formatting but didn't
switch to the native journald socket. The final two commits are the effective change.

---

## M1. TTY detection is the correct gating mechanism

**Files:** all three `main.rs` files

```rust
let interactive = std::io::IsTerminal::is_terminal(&std::io::stderr());
```

The commit message for the reverted 2f3edb8 correctly identifies the problem:
`JOURNAL_STREAM` is inherited by child processes, so terminals launched as systemd
user services (common on KDE Plasma/Wayland) pass it to `cargo run`, stripping
timestamps even in development.

`IsTerminal::is_terminal(&stderr())` is the right replacement:
- Under systemd: stderr is a pipe/socket to journald → `false` → use journald layer
- In a terminal (`cargo run`, VS Code): stderr is a TTY → `true` → use fmt layer
- Piped to a file (`tisvlayd > log.txt 2>&1`): → `false` → fmt without timestamps

The only edge case is `ssh` sessions running the daemon manually (stderr is a PTY →
`true` → fmt output), which is the correct behavior for interactive debugging.

**Severity:** Info (correct design)

---

## M2. Fallback chain is robust

**File:** [crates/tisvlayd/src/main.rs:57-68](crates/tisvlayd/src/main.rs#L57-L68)

```rust
if !interactive {
    if let Ok(journald) = tracing_journald::layer() {
        // native journald socket
    } else {
        // fallback: fmt without timestamps
    }
} else {
    // interactive: full fmt with timestamps
}
```

Three-tier fallback:
1. Non-TTY + journald socket available → native journald (structured fields, queryable)
2. Non-TTY + no socket (e.g., container without systemd) → fmt sans timestamps
3. TTY (interactive) → full fmt with timestamps

The `tracing_journald::layer()` constructor probes the socket on construction (sends
an empty payload per the source at `lib.rs:120-122`), so the `Err` path handles both
"no systemd" and "socket permission denied" cases. Good.

**Severity:** Info (correct fallback chain)

---

## M3. `EnvFilter` applies correctly to the journald layer

The `EnvFilter` is constructed once and applied to whichever layer is chosen. Since
`EnvFilter` implements `tracing_subscriber::Layer`, it filters events before they
reach the journald layer — the `zbus=warn` and crate-level directives work identically
in all three paths. Verified by reading the tracing-subscriber source: `EnvFilter`
gates `on_event` calls regardless of the downstream layer type.

**Severity:** Info (correct composition)

---

## M4. Structured fields are now queryable via `journalctl`

The `tracing-journald` crate maps tracing span/event fields to journal fields with
a configurable prefix (default `F`). This means `tv_id`, `action`, `error`, etc. are
now queryable:

```bash
journalctl -t tisvlayd F_TV_ID=living_room
journalctl -t tisvlay-session F_ACTION=screen_off
```

This is a meaningful observability improvement over the previous approach (formatting
fields into the `MESSAGE` string), especially for production debugging with multi-TV
setups.

**Severity:** Info (improvement, not an issue)

---

## M5. Duplicated init pattern across three binaries (L8 re-raised)

**Files:**
- [crates/tisvlayd/src/main.rs:51-74](crates/tisvlayd/src/main.rs#L51-L74)
- [crates/tisvlay-session/src/main.rs:200-224](crates/tisvlay-session/src/main.rs#L200-L224)
- [crates/tisvlay-tray/src/main.rs:39-63](crates/tisvlay-tray/src/main.rs#L39-L63)

This was noted as L8 in the previous review. The switch to `tracing-journald` makes
the pattern slightly more complex (three-way branch instead of two-way), increasing
the maintenance cost of the duplication. All three implementations are identical except
for the `EnvFilter` directives.

A shared helper taking a list of directive strings would now save ~20 lines per binary
and prevent drift. However, it would add `tracing-journald` and `tracing-subscriber`
as dependencies of `tisvlay-core` (currently only binaries depend on them).

Alternative: a small `tisvlay-logging` internal crate with just the init function.
This avoids polluting `tisvlay-core` with logging plumbing.

**Severity:** Low (duplication is manageable but growing; extraction is now more justified)

---

## M6. `nix/package.nix` may need `cargoHash` update

The `Cargo.lock` diff shows `tracing-journald 0.3.1` was added. If the nix build
uses a pinned `cargoHash`, it will fail until the hash is updated. This isn't a code
issue but a packaging concern the author should verify before merging.

**Severity:** Low (build/packaging, not code — verify before merge)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| M1 | **Info** | TTY detection is correct replacement for `JOURNAL_STREAM` |
| M2 | **Info** | Three-tier fallback (journald/fmt-no-time/fmt) is robust |
| M3 | **Info** | `EnvFilter` composes correctly with journald layer |
| M4 | **Info** | Structured fields now queryable via `journalctl` — observability win |
| M5 | **Low** | Duplicated init pattern now 3-way branch across 3 binaries (L8 re-raised) |
| M6 | **Low** | Verify `cargoHash` in `nix/package.nix` for new dependency |

## Overall Assessment

Clean adoption of `tracing-journald`. The TTY gating (replacing `JOURNAL_STREAM` env
var) correctly handles the KDE Plasma/Wayland session inheritance issue. The fallback
chain is robust — no journald socket means graceful degradation to fmt. Structured
journal fields are a real observability improvement for production multi-TV debugging.

The commit sequence shows good iteration: the initial `JOURNAL_STREAM` → TTY approach
was reverted and reworked with the proper journald layer in the same session, avoiding
a half-measure landing on the branch.

No actionable items beyond the two low-severity notes (M5 duplication, M6 nix hash).

---
---

# Review: Logging extraction & cargoHash update (200c75e, 81a4397)

Review of 2 commits that address **M5** (duplicated init pattern) and **M6**
(cargoHash for new dependency) from the previous review.

Build: clean. Tests: all pass.

---

## N1. `logging::init()` — clean extraction, correct feature gating

**File:** [crates/tisvlay-core/src/logging.rs](crates/tisvlay-core/src/logging.rs)

The full 3-way init logic (journald / fmt-no-time / fmt) is extracted into a single
`pub fn init(directives: &[&str]) -> anyhow::Result<()>`. Each binary now calls:

```rust
tisvlay_core::logging::init(&["tisvlayd=info", "zbus=warn"])?;
```

This directly resolves M5. The implementation is identical to the prior inlined code —
no behavioral changes, just deduplication. Net result: -93 lines across the three
binaries, +47 lines in the new module.

The `logging` feature flag is the right approach: `tisvlay-core` is also used by the
CLI (which doesn't need journald or tracing-subscriber), so making the deps optional
prevents pulling them in where they're not needed.

**Severity:** Info (correct implementation of M5)

---

## N2. Feature flag makes `tracing-subscriber` and `tracing-journald` optional on core

**File:** [crates/tisvlay-core/Cargo.toml](crates/tisvlay-core/Cargo.toml)

```toml
[features]
default = []
dbus = ["dep:zbus", "dep:anyhow"]
logging = ["dep:tracing-subscriber", "dep:tracing-journald"]
```

The three service binaries enable `features = ["logging"]` (plus `"dbus"` where
needed). The CLI (`tisvlay-cli`) does not enable `logging` — it only depends on
`tisvlay-core` with `features = ["dbus"]`, so it doesn't pull in the subscriber
or journald crates. This is the correct dependency boundary.

**Severity:** Info (correct feature design)

---

## N3. `anyhow` dependency for the logging module

**File:** [crates/tisvlay-core/src/logging.rs:20](crates/tisvlay-core/src/logging.rs#L20)

```rust
pub fn init(directives: &[&str]) -> anyhow::Result<()> {
```

The function returns `anyhow::Result`, but `anyhow` is gated behind the `dbus` feature,
not `logging`. This works today because all three binaries that enable `logging` also
enable `dbus` (which brings in `anyhow`). But if a future binary enabled only `logging`
without `dbus`, it would fail to compile.

Options:
1. Add `anyhow` to the `logging` feature's deps: `logging = ["dep:tracing-subscriber", "dep:tracing-journald", "dep:anyhow"]`
2. Return `Result<(), Box<dyn std::error::Error>>` instead — avoids coupling to anyhow
3. Accept the implicit dependency since all service binaries need both features anyway

Option 1 is the minimal correct fix. Option 3 is pragmatic but fragile.

**Severity:** Low (compiles today but the feature is technically incomplete)

---

## N4. `tisvlayd` doesn't enable `dbus` feature on `tisvlay-core`

**File:** [crates/tisvlayd/Cargo.toml](crates/tisvlayd/Cargo.toml)

```toml
tisvlay-core = { workspace = true, features = ["logging"] }
```

`tisvlayd` only enables `logging`, not `dbus`. This is correct — the daemon doesn't
use `DaemonClient` (it *is* the daemon), so it doesn't need the D-Bus client proxy.
It gets `anyhow` via its own direct dependency, not through `tisvlay-core`'s feature.

However, this means the `logging` module's `anyhow::Result` return type compiles
because `tisvlayd` has its own `anyhow` dep, not because `tisvlay-core` re-exports it.
This reinforces N3: if someone tried to use `tisvlay-core` with only `logging` enabled
and no external `anyhow`, it would fail.

**Severity:** Info (reinforces N3; not a separate issue)

---

## N5. `cargoHash` updated — M6 resolved

**File:** [nix/package.nix](nix/package.nix)

The hash is updated to account for the new `tracing-journald` dependency in
`Cargo.lock`. This directly resolves M6.

**Severity:** Info (M6 resolved)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| N1 | **Info** | Logging extraction is clean; M5 resolved |
| N2 | **Info** | Feature gating is correct; CLI doesn't pull logging deps |
| N3 | **Low** | `logging` feature should declare `anyhow` dep for completeness |
| N4 | **Info** | tisvlayd compiles because of its own anyhow dep, not core's |
| N5 | **Info** | cargoHash updated; M6 resolved |

## Overall Assessment

Both M5 and M6 from the previous review are resolved. The extraction is clean and
the feature gating is well-designed. The only minor gap is N3: the `logging` feature
doesn't declare its `anyhow` dependency, relying on callers to bring it in via
either `dbus` or their own direct dep. Adding `"dep:anyhow"` to the `logging`
feature list would make it self-contained.
