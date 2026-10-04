# MSRV verification job

`rust-toolchain.toml` pins the dev/CI channel to an exact stable release for reproducibility, and `Cargo.toml` declares `rust-version` as the MSRV floor: the oldest rustc the workspace actually builds on, below the pin. The floor has a hard ceiling too — the rustc nixos-unstable ships — because the Nix package (and so the NixOS module) builds with the consumer's nixpkgs compiler, and Cargo refuses any toolchain below `rust-version`. CI only builds on the pinned channel, so nothing verifies either bound: a contributor uses an API stabilized after the floor, CI stays green, and the `rust-version` claim is quietly false; or the floor is raised past nixpkgs and every NixOS rebuild fails.

The second case is not hypothetical: the floor was once raised to match a newer pin, and every NixOS rebuild failed with `rustc … is not supported`. The code itself compiled on a much older compiler; only the declared floor was wrong.

## Verifying locally

Take the floor toolchain from rust-overlay — the same way the devShell gets its pinned compiler — and put it first on `PATH` inside the devShell, which supplies the system libraries:

```sh
tc=$(nix build --no-link --print-out-paths --impure --expr 'let f = builtins.getFlake (toString ./.); pkgs = import f.inputs.nixpkgs { system = builtins.currentSystem; overlays = [ f.inputs.rust-overlay.overlays.default ]; }; in pkgs.rust-bin.stable."<floor>.0".minimal')
nix develop -c sh -c "export PATH=$tc/bin:\$PATH; rustc --version; cargo check --workspace --all-targets --all-features --ignore-rust-version --target-dir target/msrv"
```

Two traps on NixOS: `rustup run <ver>` inside the devShell still resolves the devShell's own cargo/rustc (rustup works through proxy binaries the devShell's real toolchain shadows), and rustup-installed toolchains fail on `slint-macros` with `E0463 can't find crate` at *every* version, including ones that demonstrably work. rustup-based tools (cargo-msrv, cargo-hack) inherit both, so they belong on a rustup-native CI runner, not in the devShell.

## What this would buy

A guard that keeps `rust-version` honest for the one consumer it has today — NixOS module users — and for crates.io consumers once a crate publishes. It also catches transitive-dep MSRV creep: if a dependency raises its own floor past nixpkgs' rustc, the check surfaces it, prompting a deliberate decision rather than a broken rebuild.

## Path forward

Three shapes, in increasing cost:

1. **A floor check job** — `cargo hack check --rust-version --workspace --all-targets --all-features` on the rustup-based CI runner. Checks each crate on its declared `rust-version`, fetching the toolchain itself; catches code or a dependency bump that outgrows the floor.
2. **Build the Nix package in CI** — `nix build .#tisvlay`, which compiles with the flake's locked nixpkgs rustc, the very compiler consumers use. Guards the ceiling (a floor raised past nixpkgs fails here); the cost is a Nix-enabled runner and a full release build (no binary cache).
3. **`cargo-msrv`** — `cargo msrv find` re-derives the true floor by bisecting toolchains. Heavier (multiple toolchain fetches); an occasional manual audit when choosing a new floor, not a per-PR gate.

Options 1 and 2 guard opposite bounds; start with 1 (cheap), add 2 if the ceiling bites again.

## Trade-offs to budget for

- **Build time** — a full workspace release build per run (option 1) or a second toolchain plus a `check` pass (option 2). Modest, but not free on a 2-core runner.
- **Verifies our code, not the whole graph at runtime** — a build proves it compiles on the floor; it does not run tests there. Adequate for the compile claim.

## Trigger conditions to re-open

Pick this up when **any** of these fires:

- CI is re-enabled on the repo — the job slots in next to the existing workflows.
- A workspace crate flips to `publish = true` and ships to crates.io — the MSRV becomes a consumed contract beyond Nix.
- Another rebuild failure on the declared floor.

## Out of scope

- Auto-bumping `rust-version` — the job surfaces a floor break; choosing the new floor stays a deliberate edit.
- A full test matrix on the MSRV toolchain — a build is the proportionate guard for the compile claim.
