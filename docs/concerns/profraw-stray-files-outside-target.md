## Stray `.profraw` files outside `target/` from instrumented binaries

**Status**: mitigated by workspace `.cargo/config.toml`
**Severity**: low — cosmetic working-tree noise, no functional impact
**Found by**: post-merge audit of working-tree artifacts, 2026-05

### Symptom

After running tests (`just test`, `prek run -a`, IDE test actions, etc.), `git status` shows hundreds of untracked files matching `default_<hash>_<pid>.profraw` scattered across crate directories — most commonly `crates/tisvlay-cli/`, `crates/tisvlay-core/`, `crates/tisvlay-lg/`, `crates/tisvlay-tray/`. The files survive `cargo clean` and `rm -rf target/`. Naively gitignoring them would mask the leak rather than fix it; users who delete `target/` reasonably expect all build-time artifacts to be gone.

### Layered root cause

The leak is the result of four layers each making a locally reasonable choice:

| Layer | Behavior | Why this is reasonable in isolation |
|---|---|---|
| **compiler-rt** (LLVM's coverage runtime) | When `LLVM_PROFILE_FILE` is unset, write `default_<hash>_<pid>.profraw` to the running binary's cwd. | It's a generic library linked into anything LLVM compiles; can't know about cargo workspaces. Writing to cwd is a reasonable last-resort default for an arbitrary instrumented executable. |
| **rustc** | When `-C instrument-coverage` is set, link in compiler-rt's profile runtime. Don't touch env vars. | rustc is a compiler, not a workflow tool. |
| **cargo** | Build and orchestrate; do not introspect produced binaries for instrumentation symbols, do not auto-set `LLVM_PROFILE_FILE`. | Cargo treats coverage as just one of many possible `RUSTFLAGS` effects. Auto-defaulting `LLVM_PROFILE_FILE` would couple cargo to LLVM's coverage runtime specifically (see [internals.rust-lang.org pre-RFC](https://internals.rust-lang.org/t/pre-rfc-rust-code-coverage-integration-in-cargo/16758) where `mathstuf` argued explicitly against this coupling). |
| **cargo-llvm-cov** (third-party) | Set `LLVM_PROFILE_FILE` and `CARGO_TARGET_DIR=target/llvm-cov-target/` before invoking cargo. | This is the layer that *does* the workflow ergonomics — but it can only protect its own invocations. |

The leak path: a past `cargo llvm-cov` invocation (or manual `RUSTFLAGS=-C instrument-coverage cargo test`) that didn't isolate to `target/llvm-cov-target/` leaves instrumented test binaries in `target/debug/deps/`. Cargo's fingerprint eventually triggers fresh uninstrumented sibling builds, but does *not* garbage-collect the old instrumented ones — they sit alongside the new ones with different hash suffixes. When *anything* outside the cargo-llvm-cov flow runs them — plain `cargo test`, rust-analyzer test actions, doctest harnesses, a CI step that didn't go through the recipe — `LLVM_PROFILE_FILE` is unset and compiler-rt's cwd default kicks in.

### How to confirm a binary is instrumented

```bash
nm target/debug/deps/<crate>-<hash> 2>/dev/null | grep __llvm_profile_runtime
```

Output `1` (one symbol) → instrumented. Output `0` or empty → uninstrumented. The instrumented siblings to fresh uninstrumented binaries can coexist for arbitrarily long since cargo doesn't garbage-collect by fingerprint mismatch.

### The mitigation

`.cargo/config.toml`:

```toml
[env]
LLVM_PROFILE_FILE = { value = "target/profraw/default_%m_%p.profraw", relative = true }
```

Why this works:

- **Cargo `[env]` is a default**, applied only when the var isn't already set in the calling shell ([cargo book reference](https://doc.rust-lang.org/cargo/reference/config.html#env)). So `cargo llvm-cov` (and `just cov` / `just test-ci` recipes that `eval "$(cargo llvm-cov show-env --sh)"`) override this with their own `LLVM_PROFILE_FILE` — no conflict with the actual coverage pipeline.
- **`relative = true`** resolves the path against the workspace root (the directory containing `.cargo/config.toml`), so the env var that propagates to test-binary subprocesses is an absolute path.
- **`%m` (module hash) and `%p` (pid)** are compiler-rt's standard substitutions; they prevent collisions across parallel test processes.
- **Uninstrumented binaries inherit the env var harmlessly** — only the compiler-rt runtime, which they don't link, consults it.

Net effect: any stray instrumented run lands under `target/profraw/` instead of polluting per-crate directories. The "delete `target/` to clean everything" invariant is restored.

### Why this isn't "just a cargo bug"

Cargo's choice not to auto-set `LLVM_PROFILE_FILE` is locally defensible — it's part of cargo's broader posture of not coupling to specific `RUSTFLAGS` effects. The ergonomics gap is a *layering* problem: no single layer has both the workspace context and the instrumentation knowledge to decide where output should go, so it falls to the user (or coverage-tool wrapper) to wire them together.

### Upstream survey

Researched 2026-05. Key issues / discussions, organized by what they're actually about:

| Issue | Status | Relevance |
|---|---|---|
| [rust-lang/rfcs#3287](https://github.com/rust-lang/rfcs/pull/3287) — Add native code coverage support in Cargo | **Closed-postponed** by `@ehuss` 2024-03 | Lacked a champion; team explicitly invited unstable-feature experimentation as the path forward, but nobody has driven it. RFC discussion (`@jonhoo`) flagged the artifact-clobber problem (coverage and non-coverage builds sharing `target/`) — the author conceded a separate target dir would be required, exactly what cargo-llvm-cov already does. |
| [rust-lang/cargo#13040](https://github.com/rust-lang/cargo/issues/13040) — Native cargo coverage support | **Open, S-needs-rfc** | Umbrella tracking issue. Scoped to *instrumentation selection* (e.g., `[profile.dev] instrument-coverage = true`), not output-path defaults. Closest upstream issue to subscribe to, but won't ship this fix specifically. |
| [rust-lang/cargo#13136](https://github.com/rust-lang/cargo/issues/13136) — Garbage collect whole `target/` | **Open**, no implementation | Would address the lingering-instrumented-binary co-cause indirectly. |
| [rust-lang/rust#100381](https://github.com/rust-lang/rust/issues/100381) + [PR #100384](https://github.com/rust-lang/rust/pull/100384) — Unique profraw filenames | **Closed/fixed** 2022-08 | Red herring for this concern. Only changed the *filename* pattern (added `%m_%p` substitutions) so concurrent test processes don't clobber each other; *did not* change the output directory. The cwd default is still the cwd default. |
| [rust-lang/rust#79899](https://github.com/rust-lang/rust/issues/79899) — Output filenames for `-Z instrument-coverage` | Open, no team commentary | Same scope as #100381 — filename, not directory. |
| [Internals pre-RFC, June 2022](https://internals.rust-lang.org/t/pre-rfc-rust-code-coverage-integration-in-cargo/16758) | Auto-closed after 90 days | Author proposed cargo set `LLVM_PROFILE_FILE` with unique patterns. `mathstuf` pushed back on cargo exposing the envvar at all. Fed into RFC #3287. |

**Bottom line**: there is no upstream consensus that cargo should ship this defaulting behavior, and there's no single open issue specifically asking for it that's worth subscribing to. The closest team-adjacent voice (mathstuf on internals) actively *opposed* the coupling. The workspace-level `.cargo/config.toml` `[env]` workaround is the canonical user-side fix the upstream discussions point to.

### Comparison with cargo-tarpaulin

The other major Rust coverage tool, [cargo-tarpaulin](https://github.com/xd009642/tarpaulin), sidesteps this gap entirely by **owning the test harness**: it sets the target dir, orchestrates execution itself, and doesn't generate stray cwd profraw files under normal use. The leak path described above doesn't apply because tarpaulin's instrumented binaries live in its own target subdir and aren't reused by plain `cargo test`. cargo-llvm-cov's layering — instrument the workspace's existing target dir and rely on env vars — is what makes it vulnerable to this specific leak path.

This is a layering trade-off, not a bug in cargo-llvm-cov: tarpaulin's approach is more invasive (it needs ptrace and Linux-specific orchestration), and cargo-llvm-cov's portability advantage comes from not owning the harness. Both choices are defensible.

### Re-evaluation triggers

Patterns to revisit when their gating condition fires:

- **Cargo cross-workspace-cache 2026 effort lands** — if cargo learns to share `target/` across different `RUSTFLAGS` (the [2026 goal](https://rust-lang.github.io/rust-project-goals/2026/cargo-cross-workspace-cache.html)), the show-env workaround becomes deletable AND the lingering-instrumented-binary co-cause likely goes away too. The `.cargo/config.toml` env default would still be useful as belt-and-suspenders, but the leak surface shrinks.
- **rust-lang/cargo#13040 progresses past S-needs-rfc** — if cargo lands native coverage support, the env-defaulting question is back on the table. Re-evaluate whether the workspace workaround should be replaced by the upstream feature.
- **rust-lang/cargo#13136 lands** — if cargo learns to garbage-collect `target/debug/deps/` by fingerprint, lingering instrumented binaries stop accumulating. The env-default workaround is still useful (defense in depth) but the leak frequency drops.
- **cargo-llvm-cov upstream change** — if cargo-llvm-cov ever stops exporting its own `LLVM_PROFILE_FILE` (unlikely), the precedence-based shadowing this concern relies on would need to be re-validated. The fix would then have to use `force = true` on the cargo `[env]` entry, but that would *break* `cargo llvm-cov`. So the trigger is "does cargo-llvm-cov still export `LLVM_PROFILE_FILE` from `show-env`?" — currently yes, stable since 0.6.

### References

- [`/.cargo/config.toml`](../../.cargo/config.toml) — the `[env]` block where the default lives, with inline rationale.
- [Cargo Book: `[env]` table](https://doc.rust-lang.org/cargo/reference/config.html#env) — `relative = true` and precedence-vs-shell-env documentation.
- [rustc Book: Instrumentation-based Code Coverage](https://doc.rust-lang.org/rustc/instrument-coverage.html) — the `LLVM_PROFILE_FILE` env var and its `%m`/`%p` substitutions.
- [taiki-e/cargo-llvm-cov `show-env`](https://github.com/taiki-e/cargo-llvm-cov?tab=readme-ov-file#get-coverage-of-cc-code-linked-to-rust-libtest-binary) — the upstream API our pipeline depends on.
- Sibling concern: [CI test-ci pipeline depends on `cargo llvm-cov show-env` workaround](../concerns.md#ci-test-ci-pipeline-depends-on-cargo-llvm-cov-show-env-workaround) — same root cause, different surface.
