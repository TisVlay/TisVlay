# MSRV verification job

`rust-toolchain.toml` pins the dev/CI channel to an exact stable release for reproducibility, and `Cargo.toml` declares `rust-version` as the MSRV floor. The floor currently equals the pinned channel (the two are bumped together), so CI on the pinned channel already proves it — this job only matters once the floor is deliberately set below the channel. Should that happen, nothing verifies the workspace actually compiles on that floor, so the declared MSRV can drift silently: a contributor uses an API stabilized after the floor, CI on the pinned channel stays green, and the `rust-version` claim is quietly false.

Deferred while the workspace is unpublished — every crate is `publish = false` except the few marked publish-ready, so no downstream consumer builds against the floor yet. The claim only becomes load-bearing when a crate ships to crates.io.

## What this would buy

A cheap guard that keeps `rust-version` honest, so the first crate we publish doesn't discover a broken MSRV at release time. It also catches transitive-dep MSRV creep: if a dependency raises its own floor, a check on the pinned MSRV surfaces it, prompting a deliberate `rust-version` bump rather than a silent contract break.

## Path forward

Two shapes, in increasing cost:

1. **A dedicated CI job** — `cargo check --workspace --all-features` on the MSRV toolchain, provisioned the same way as the pinned channel (a second rust-overlay pin, or `dtolnay/rust-toolchain@<msrv>`). Kept off the instrumented `just test-ci` path (it is a compile gate, not a coverage build). Required or `continue-on-error` depending on how load-bearing the claim is at the time.
2. **`cargo-msrv`** — auto-discovers/verifies the true floor by bisecting toolchains. Heavier (multiple toolchain fetches); better as an occasional manual audit to re-derive `rust-version` than as a per-PR gate.

Start with option 1 as a non-blocking job; promote to required (and/or add the option-2 audit) when a crate actually publishes.

## Trade-offs to budget for

- **A second toolchain in CI** — download plus a full-workspace `check` pass. Modest, but not free on a 2-core runner.
- **Verifies our code, not the whole graph at runtime** — a `check` proves it compiles on the floor; it does not run tests there. Adequate for an MSRV claim, not a full MSRV test matrix.
- **Low value while unpublished** — nobody consumes the `rust-version` field yet, so the gate guards a contract with no counterparty until the first publish.

## Trigger conditions to re-open

Pick this up when **any** of these fires:

- A workspace crate flips to `publish = true` and ships to crates.io — the MSRV becomes a consumed contract.
- A contributor (or a dependency bump) reports a build failure on the declared floor.
- The pinned dev/CI channel drifts far enough above the floor that the untested gap feels risky.

## Out of scope

- Auto-bumping `rust-version` — the job surfaces a floor break; choosing the new floor stays a deliberate edit.
- A full test matrix on the MSRV toolchain — a `check` is the proportionate guard for the compile claim.
