# Shell completions for `tisvlay`

## Context

Users who install TisVlay via a distro package (`.deb`/`.rpm`/`.pkg.tar.zst`) or the Nix flake get no terminal tab-completion for the `tisvlay` CLI. The CLI is the one interactive binary in the workspace, and its most awkward gap is that nearly every device verb (`set-volume`, `input`, `power`, `config`, …) takes a bare `<Uuid>` positional that is impossible to type from memory. We want completion that is present after install with **no user post-install setup**, and that can complete **live TV UUIDs** — the Docker experience, where `/usr/share/bash-completion/completions/docker` ships in the package and completes live container names by calling back into the binary.

Scope: the `tisvlay` CLI (crate `tisvlay-cli`) only. The daemon/session/tray/greeter/gui are systemd- or desktop-launched services with near-empty arg surfaces and are out of scope.

## Approach: clap_complete dynamic engine + shipped bootstrap files

Use `clap_complete`'s dynamic completion engine (`env::CompleteEnv`, feature `unstable-dynamic`). At package-build time we capture the per-shell **registration script** the binary emits and ship it into each shell's standard autoload directory. The shipped script is a self-contained bootstrap: on `<TAB>` it calls back into `tisvlay` (with `COMPLETE=<shell>` set inline) to compute candidates, including live TV UUIDs. This gives zero-config completion for subcommands, flags, fixed value-sets (`on`/`off`), **and** runtime state, from one mechanism.

Why dynamic over the static `aot` generators: static files cannot complete runtime state (TV UUIDs), which is the CLI's biggest ergonomic gap. The dynamic engine is a strict superset — it still completes subcommands/flags/value-sets — so there is no reason to also generate static files. Accepted cost: `unstable-dynamic` is an explicitly unstable, churning API; on a `clap_complete` bump we regenerate the files (already a build step) and fix any moved call sites.

## Implementation

### 1. Dependencies

- Add to `[workspace.dependencies]` (root `Cargo.toml`): `clap_complete = { version = "4", features = ["unstable-dynamic"] }`.
- `crates/tisvlay-cli/Cargo.toml`: add `clap_complete = { workspace = true }`, and enable the extension attribute on clap for this crate: `clap = { workspace = true, features = ["unstable-ext"] }` (needed for the derive `#[arg(add = …)]` value-candidate attribute; `unstable-dynamic` pulls `clap/unstable-ext` transitively but naming it on the crate keeps the requirement explicit).

### 2. CLI wiring — `crates/tisvlay-cli/src/main.rs`

- Drop `#[tokio::main]`. Make `main` a plain sync fn so the completion path runs **before** any tokio runtime exists:

  ```rust
  fn main() -> anyhow::Result<()> {
      clap_complete::CompleteEnv::with_factory(Cli::command)
          .completer("tisvlay")   // PATH-resolved callback; portable across deb/rpm/nix
          .complete();            // computes candidates and exits(0) iff COMPLETE is set
      tokio::runtime::Builder::new_multi_thread()
          .enable_all()
          .build()?
          .block_on(run())        // `run()` = today's async main body, renamed
  }
  ```

  Rationale: `CompleteEnv::complete()` returns immediately on a normal run (COMPLETE unset) and `exit(0)`s on a completion request. Running it before the runtime is built means (a) completion subprocesses never touch zenoh unless a UUID candidate closure fires, and (b) those closures run with **no ambient runtime**, so each can build its own short-lived `multi_thread` runtime for the daemon query (zenoh rejects the current-thread scheduler). Requires `use clap::CommandFactory` for `Cli::command`.

### 3. Live TV-UUID candidates

- Add a shared, best-effort, time-bounded helper that returns `Vec<CompletionCandidate>` and **degrades to empty on any failure so completion never hangs**:

  ```rust
  fn tv_candidates() -> Vec<clap_complete::CompletionCandidate> {
      let Ok(rt) = tokio::runtime::Builder::new_multi_thread().enable_all().build() else { return vec![] };
      rt.block_on(async {
          // open a client session on default_connect_socket_path(), subscribe
          // keys::tv_all(site) with a short hard timeout (~300–500 ms), collect
          // (uuid -> meta/identity.name), return one candidate per TV with the
          // name as help text. Any error / no daemon -> empty Vec.
      }).unwrap_or_default()
  }
  ```

  Reuse the hydration shape already in `list()` (`declare_subscriber(keys::tv_all(site)).history(…)`, drain with a `tokio::time::timeout` quiet window) but with a tighter deadline and no printing.
- Attach it to the TV-referencing `Uuid` args via the derive attribute, e.g. `#[arg(add = clap_complete::engine::ArgValueCandidates::new(tv_candidates))]` on the `id`/`tv` fields of the relevant `Command`, `ProfileCmd`, and `UserProfileCmd` variants (`SetVolume`, `Input`, `Backlight`, `Screen`, `Rename`, `Managed`, `Config`, `ConfigSet`, `Repair`, `Power`, `Reconnect`, `Remove`, `Mute`, `PrimaryInput`, `Assign`, `Effective`, …). Mechanical, repeated across ~15 sites.
- Out of scope for v1 (easy follow-ups): completing profile UUIDs (`profile: Option<Uuid>`, `ProfileCmd`/`UserProfileCmd` ids) via a `profile_candidates()`, and vendor names for `--vendor`.

### 4. Build-time generation — `justfile`

- New recipe `completions` (mirrors `licenses-html`): build `tisvlay-cli` **host-native** (a normal debug build — the cross-compiled `zigbuild` artifact has a `/lib64/ld-linux…` interpreter that won't run on the NixOS build host), then run it three times with `COMPLETE` set, writing into a gitignored dir:
  - `COMPLETE=bash target/debug/tisvlay > packaging/completions/tisvlay.bash`
  - `COMPLETE=zsh  target/debug/tisvlay > packaging/completions/tisvlay.zsh`
  - `COMPLETE=fish target/debug/tisvlay > packaging/completions/tisvlay.fish`
- Make `completions` a prerequisite of `package-deb`, `package-rpm`, `package-archlinux` (alongside `licenses-html`).
- `.gitignore`: add `/packaging/completions/` (generated artifact, like `THIRD-PARTY-LICENSES.html`).

### 5. nfpm packaging — `crates/tisvlay-daemon/Cargo.toml`

Add three entries to `[package.metadata.nfpm].contents`, mapping the generated files to each shell's **autoload** path (note the shell-native destination names):

```toml
{ src = "packaging/completions/tisvlay.bash", dst = "/usr/share/bash-completion/completions/tisvlay" },
{ src = "packaging/completions/tisvlay.zsh",  dst = "/usr/share/zsh/site-functions/_tisvlay" },
{ src = "packaging/completions/tisvlay.fish", dst = "/usr/share/fish/vendor_completions.d/tisvlay.fish" },
```

No new `depends`: completion files are a passive enhancement; do not hard-depend on `bash-completion`.

### 6. Nix packaging — `packaging/nix/package.nix`

- Add `installShellFiles` to `nativeBuildInputs`.
- In `postInstall`, drive the same mechanism through the nix-native `$out/bin/tisvlay` (runs fine in the builder; `installShellCompletion` renames/places into `$out/share/…`):

  ```sh
  installShellCompletion --cmd tisvlay \
    --bash <(COMPLETE=bash $out/bin/tisvlay) \
    --zsh  <(COMPLETE=zsh  $out/bin/tisvlay) \
    --fish <(COMPLETE=fish $out/bin/tisvlay)
  ```

- No `module.nix` change: with the package on the system profile, the completion dirs land on the paths bash-completion/`compinit`/fish scan. (Users still need their shell's completion enabled — `programs.bash.completion.enable`, etc. — which is their config.)

### 7. Licensing gate

`clap_complete` (MIT OR Apache-2.0) and its new transitive deps (`shlex`, `is_executable`) enter the graph. After wiring, run `just licenses-html` and add `about.toml` entries only if the `--all-features --fail` gate flags any (e.g. `is_executable` is ISC). Low risk; all permissive.

### 8. Docs

- `packaging/README.md`: a short "Shell completions" subsection — the dynamic-bootstrap model, the three install paths, the host-native generation subtlety, and the manual-install escape hatch for unsupported distros (`COMPLETE=bash tisvlay > ~/.local/share/bash-completion/completions/tisvlay`).
- `.claude/CLAUDE.md` Quick Reference: one line noting completions ship in packages/nix.

## Decisions

- **No `completions <shell>` subcommand.** With the dynamic model the generation entry point is `COMPLETE=<shell> tisvlay`; a subcommand would call the static `aot` `generate` (no live values), inconsistent with the shipped bootstrap. Manual install is documented via the env invocation instead.
- **Callback pinned to bare `tisvlay`** (`.completer("tisvlay")`) so the shipped script is PATH-portable across all channels.

## Risks / caveats

- `unstable-dynamic` API churn — contained to the CLI crate; regenerate + fix call sites on bump.
- Every `<TAB>` spawns `tisvlay` as a subprocess; only the UUID-arg case pays a daemon round-trip, guarded by a hard timeout and empty-on-failure so a downed daemon never hangs the prompt.
- Per-shell autoload naming must be exact (bash: bare `tisvlay`; zsh: `_tisvlay` with leading `#compdef`; fish: `tisvlay.fish`) — verified against the 4.6.7 templates.

## Verification

1. `just completions` → inspect `packaging/completions/tisvlay.bash` contains `complete -F _clap_complete_tisvlay tisvlay`; `tisvlay.zsh` starts with `#compdef tisvlay`.
2. `just check` (build/clippy/test green; existing `clap_definition_is_consistent` test still passes). Add a unit test asserting `tv_candidates()` returns `vec![]` promptly when no daemon is reachable.
3. `just package-deb` → `dpkg -c target/…/tisvlay_*.deb` shows the three completion paths.
4. Docker smoke test (`packaging/docker`): install the `.deb`, open a fresh bash, `tisvlay <TAB>` → subcommands; with a dev daemon + a paired mock TV running, `tisvlay set-volume <TAB>` → TV UUIDs with names as descriptions; with no daemon → no candidates, no hang.
5. `nix build` → confirm `result/share/bash-completion/completions/tisvlay` (and the zsh/fish equivalents) exist and start with the registration.
