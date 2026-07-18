# Shell completions for `tisvlay`

## Context

Users who install TisVlay via a distro package (`.deb`/`.rpm`/`.pkg.tar.zst`) or the Nix flake get no terminal tab-completion for the `tisvlay` CLI. The CLI is the one interactive binary in the workspace, and its most awkward gap is that nearly every device verb (`set-volume`, `input`, `power`, `config`, …) takes a bare `<Uuid>` positional that is impossible to type from memory. We want completion that is present after install with **no user post-install setup**, and that can complete **live TV UUIDs** — the Docker experience, where `/usr/share/bash-completion/completions/docker` ships in the package and completes live container names by calling back into the binary.

Scope: the `tisvlay` CLI (crate `tisvlay-cli`) only. The daemon/session/tray/greeter/gui are systemd- or desktop-launched services with near-empty arg surfaces and are out of scope.

## Approach: clap_complete dynamic engine + shipped bootstrap files

Use `clap_complete`'s dynamic completion engine (`env::CompleteEnv`, feature `unstable-dynamic`). At package-build time we capture the per-shell **registration script** the binary emits and ship it into each shell's standard autoload directory. The shipped script is a self-contained bootstrap: on `<TAB>` it calls back into `tisvlay` (with `COMPLETE=<shell>` set inline) to compute candidates, including live TV UUIDs. This gives zero-config completion for subcommands, flags, fixed value-sets (`on`/`off`), **and** runtime state, from one mechanism.

Why dynamic over the static `aot` generators: static files cannot complete runtime state (TV UUIDs), the CLI's biggest ergonomic gap. The dynamic engine is a strict superset — subcommand/flag/value-set completion is computed in-process from the `Command` tree and never touches the daemon; only a value-candidate closure runs custom code. So with the daemon down, completion already degrades to full static-equivalent behavior minus live UUIDs, and there is no reason to also generate static files. Accepted cost: `unstable-dynamic` is an explicitly unstable, churning API; on a bump we regenerate the files (already a build step) and fix any moved call sites — localized to the CLI crate.

## Implementation

### 1. Dependencies

- Add to `[workspace.dependencies]` (root `Cargo.toml`): `clap_complete = { version = "4.6", features = ["unstable-dynamic"] }`. Minor-anchored (not bare `"4"`), mirroring the `zenoh = "1.9"` precedent for a dep whose `unstable` surface churns — a surprise minor jump on `cargo update` shouldn't silently pull a changed engine; the committed `Cargo.lock` (nix builds `--locked`) still provides reproducibility, and no `=` exact pin (against project norm).
- `crates/tisvlay-cli/Cargo.toml`: add `clap_complete = { workspace = true }`, and enable the extension attribute on clap for this crate: `clap = { workspace = true, features = ["unstable-ext"] }` (needed for the derive `#[arg(add = …)]` value-candidate attribute → `Arg::add`, which is `unstable-ext`-gated; `unstable-dynamic` pulls `clap/unstable-ext` transitively but naming it on the crate keeps the requirement explicit).

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

  Rationale (worth a concise code comment — *why* it diverges from the other binaries' `#[tokio::main]`, no plan/phase reference): `CompleteEnv::complete()` returns immediately on a normal run (COMPLETE unset) and `exit(0)`s on a completion request. Running it before the runtime is built means (a) completion subprocesses never touch zenoh unless a UUID candidate closure fires, and (b) those closures run with **no ambient runtime**, so each can build its own short-lived `multi_thread` runtime for the daemon query (zenoh rejects the current-thread scheduler) without the "cannot start a runtime from within a runtime" panic. Requires `use clap::CommandFactory` for `Cli::command`.

### 3. Live TV-UUID candidates

- Add a shared, best-effort, time-bounded helper returning `Vec<CompletionCandidate>` that **degrades to empty on any failure so completion never hangs**. The hard timeout must wrap the **whole** async block — `zenoh::open` *and* subscribe *and* drain — not just the drain: an absent socket fails fast (ENOENT), but a *wedged* daemon would make an un-timed `open()` await forever.

  ```rust
  fn tv_candidates() -> Vec<clap_complete::engine::CompletionCandidate> {
      let Ok(rt) = tokio::runtime::Builder::new_multi_thread().enable_all().build() else { return vec![] };
      rt.block_on(async {
          tokio::time::timeout(Duration::from_millis(400), async {
              // open a client session on default_connect_socket_path(), subscribe
              // keys::tv_all(<default site>) with history, drain a short quiet window,
              // collect (uuid -> meta/identity.name); one candidate per TV,
              // name as .help(...) description.
          }).await
      }).ok().flatten().unwrap_or_default()  // timeout OR error OR none -> empty
  }
  ```

  Reuse the hydration shape from `list()` (`declare_subscriber(keys::tv_all(site)).history(…)`, drain with a `tokio::time::timeout` quiet window). `CompletionCandidate::new(uuid).help(Some(name.into()))` — `.help` renders as the completion description in fish/zsh.
  - **Blind spot to document (code comment + Decisions):** `CompleteEnv::complete()` fires before clap parses argv, so `tv_candidates` cannot see the global `--socket`/`--site`/`--user-socket` flags (main.rs:18-31). It resolves the *default* socket and site only; live UUID completion therefore works against the default site. Do not "fix" this by trying to read the flags — the `ArgValueCandidates::new(fn)` closure has no access to sibling args by construction.

- Localize the unstable constructor behind one helper so a churn is a one-line change, not 18 edits:

  ```rust
  fn tv_completions() -> clap_complete::engine::ArgValueCandidates {
      clap_complete::engine::ArgValueCandidates::new(tv_candidates)
  }
  ```

- Attach `#[arg(add = tv_completions())]` to exactly the **18 TV-referencing `Uuid` fields** — enumerated so nobody misattaches to a profile-id field:
  - `Command`: `SetVolume`, `Input`, `Backlight`, `Screen`, `Rename`, `Managed`, `Config`, `ConfigSet`, `Repair`, `Power`, `Reconnect`, `Remove`, `Mute`, `PrimaryInput` (`id`), and `Assign` **`id`** (15).
  - `ProfileCmd`: `Effective` **`tv`** (1).
  - `UserProfileCmd`: `Effective` **`tv`**, `Assign` **`tv`** (2).
  - **Exclude** (these `Uuid`s are *profile* ids, not TVs — deferred `profile_candidates()` follow-up): `Command::Assign.profile`, `ProfileCmd::{Update,Duplicate,Delete}.id`, `UserProfileCmd::{Update,Duplicate,Delete}.id`, `UserProfileCmd::Assign.profile`.
- Out of scope for v1 (easy follow-ups): `profile_candidates()` for the excluded profile-id fields; vendor-name completion for `--vendor`; a `TISVLAY_COMPLETE_NO_DAEMON=1` kill switch to drop just the per-TAB daemon round-trip; a short-TTL UUID cache in `$XDG_RUNTIME_DIR/tisvlay/` (makes rapid tabs instant and lets a momentary daemon blip fall back to last-known instead of empty).

### 4. Build-time generation — `justfile`

- New recipe `completions` (mirrors `licenses-html`/`sbom`), all inline in `just` (no standalone shell script). Build `tisvlay-cli` **host-native** (a normal debug build — the cross-compiled `zigbuild` artifact has a `/lib64/ld-linux…` interpreter that won't run on the NixOS build host; the registration script is a shell-name-only template independent of build profile, so debug output is identical to release), then emit into a gitignored dir:

  ```
  cargo build -p tisvlay-cli
  mkdir -p packaging/completions
  COMPLETE=bash target/debug/tisvlay > packaging/completions/tisvlay.bash
  COMPLETE=zsh  target/debug/tisvlay > packaging/completions/tisvlay.zsh
  COMPLETE=fish target/debug/tisvlay > packaging/completions/tisvlay.fish
  ```

  (One line noting the accepted cost: this compiles a debug host-native CLI — pulling a debug zenoh build — on top of the release zigbuild for every `package-*`. Unavoidable given the dynamic-completion choice.)
- Make `completions` a prerequisite of `package-deb`, `package-rpm`, `package-archlinux` (alongside `licenses-html`).
- `.gitignore`: add `/packaging/completions/` with a comment mirroring the `THIRD-PARTY-LICENSES.html` block (generated artifact, regenerated by `just completions` before each `package-*`).

### 5. nfpm packaging — `crates/tisvlay-daemon/Cargo.toml`

Add three entries to `[package.metadata.nfpm].contents`, mapping the generated files to each shell's **autoload** path (shell-native destination names):

```toml
{ src = "packaging/completions/tisvlay.bash", dst = "/usr/share/bash-completion/completions/tisvlay" },
{ src = "packaging/completions/tisvlay.zsh",  dst = "/usr/share/zsh/site-functions/_tisvlay" },
{ src = "packaging/completions/tisvlay.fish", dst = "/usr/share/fish/vendor_completions.d/tisvlay.fish" },
```

Add a weak dependency (not a hard `depends` — completions are a passive enhancement) so the bash zero-config path works out of the box where the loader isn't preinstalled: `recommends = ["bash-completion"]` in `[package.metadata.nfpm.deb]` and `[package.metadata.nfpm.rpm]` (apt installs Recommends by default; Arch/minimal/containers don't ship bash-completion). Verify the zsh `site-functions` dst is on the default `$fpath` on Debian (Debian also ships `/usr/share/zsh/vendor-completions`; `site-functions` is normally in fpath too — confirm on a clean image before release).

### 6. Nix packaging — `packaging/nix/package.nix`

- Add `installShellFiles` to `nativeBuildInputs`.
- In `postInstall`, drive the same mechanism through the nix-native `$out/bin/tisvlay` (runs fine in the builder; `installShellCompletion` renames/places into `$out/share/…`). Add a comment mirroring the license dual-path convention — *why* nix regenerates rather than reading the gitignored `packaging/completions/` (unavailable/unreliable in the sandbox), and that debug/release bootstraps are identical:

  ```sh
  installShellCompletion --cmd tisvlay \
    --bash <(COMPLETE=bash $out/bin/tisvlay) \
    --zsh  <(COMPLETE=zsh  $out/bin/tisvlay) \
    --fish <(COMPLETE=fish $out/bin/tisvlay)
  ```

- No `module.nix` change: with the package on the system profile the completion dirs land on the paths bash-completion/`compinit`/fish scan. Document (README) that users still need their shell's completion enabled — `programs.bash.completion.enable` / `programs.zsh.enable` / `programs.fish.enable` on NixOS; `compinit` for zsh on every distro.

### 7. Licensing gate

`clap_complete` (MIT OR Apache-2.0) enters the graph. Its extra deps `shlex` and `clap_lex` are **already in-graph** (via `cc` and `clap` respectively); the only genuinely new transitive crate is `is_executable`. All are permissive and covered by `about.toml`'s existing `accepted` MIT/Apache/ISC list, so the `--all-features --fail` gate should need no new entry. After wiring, run `just licenses-html`; only if the gate flags something, add the matching `about.toml` entry and the per-crate note in `docs/licensing.md`.

### 8. Docs

- `packaging/README.md`: update the Channels-table line "all five binaries plus systemd units and the GUI desktop entry" to also mention the three shell-completion files, and add a short "Shell completions" subsection — the dynamic-bootstrap model, the three install paths, the host-native generation subtlety, the per-distro caveats (bash-completion presence, zsh `compinit`, NixOS `programs.*`), and the manual-install escape hatch for unsupported distros (`COMPLETE=bash tisvlay > ~/.local/share/bash-completion/completions/tisvlay`).
- `.claude/CLAUDE.md` Quick Reference: one line noting completions ship in packages/nix.

## Decisions

- **No `completions <shell>` subcommand.** With the dynamic model the generation entry point is `COMPLETE=<shell> tisvlay`; a subcommand would call the static `aot` `generate` (no live values), inconsistent with the shipped bootstrap. Manual install is documented via the env invocation instead.
- **Callback pinned to bare `tisvlay`** (`.completer("tisvlay")`) so the shipped script is PATH-portable across all channels (without it, generating via `target/debug/tisvlay` would bake the absolute build path into the callback).
- **Dynamic zsh, not a static-`_tisvlay` hybrid.** The `#compdef`-tagged dynamic registration autoloads from `site-functions` cleanly — validated by clap_complete's own test suite (`tests/snapshots/home/dynamic-env/exhaustive/zsh` uses fpath + `compinit` and asserts real candidates). No hybrid needed.
- **i18n: no action.** The CLI isn't gettext-wired (only `tisvlay-gui` is), and completion help text is the user's own device *name* (user data / machine identifier), which the i18n rules say not to translate. No `just i18n-extract`, no `.pot` churn.

## Risks / caveats

- `unstable-dynamic` API churn — contained to the CLI crate; regenerate + fix call sites on bump. Minor-anchored version guards against silent `cargo update` drift.
- Every `<TAB>` spawns `tisvlay` as a subprocess; only the UUID-arg case pays a daemon round-trip. Felt latency is `zenoh::open` + subscribe + hydrate + teardown per fresh process (~400–700 ms worst case), hard-timeout-bounded and empty-on-failure so a downed/wedged daemon never hangs the prompt.
- Transient version-skew window: a shell started before a package upgrade holds the old sourced/autoloaded registration but calls the new binary until it re-sources. Safe here because script and binary are co-packaged from one `clap_complete` version and upgraded atomically — noted so a future maintainer doesn't "fix" the deliberate file-shipping (upstream's "don't persist the registration" advice targets hand-caching against a drifting binary, not our atomic co-packaging).
- Zero-config is true on desktop-default installs; the caveats above (bash-completion presence, zsh `compinit`, NixOS `programs.*`) are documented, and `recommends: bash-completion` widens the bash surface.

## Verification

1. `just completions` → inspect `packaging/completions/tisvlay.bash` contains `-F _clap_complete_tisvlay tisvlay` (the emitted line carries `-o nospace -o bashdefault -o nosort` between `complete` and `-F`, so grep the `-F …` fragment, not a literal `complete -F …`); `tisvlay.zsh` starts with `#compdef tisvlay`.
2. `just check` (build/clippy/test green; existing `clap_definition_is_consistent` test still passes). Add (a) a **hermetic** test that `tv_candidates()` returns `vec![]` promptly with a bogus `XDG_RUNTIME_DIR` (run as a subprocess so env isolation is safe under nextest; assert both empty and bounded-time so the timeout is exercised), and (b) an integration test running `env COMPLETE=bash <CARGO_BIN_EXE_tisvlay>` asserting the output contains `_clap_complete_tisvlay` (and a `#compdef tisvlay` check for zsh) — this catches a `clap_complete` bump that changes the bootstrap template in CI without a shell present (`debug_assert()` does not cover the `add` wiring).
3. `just package-deb` → `dpkg -c target/…/tisvlay_*.deb` shows the three completion paths.
4. Docker smoke test (`packaging/docker`, base image with `bash-completion` present): install the `.deb`, open a fresh bash, `tisvlay <TAB>` → subcommands; with a dev daemon + a paired mock TV running, `tisvlay set-volume <TAB>` → TV UUIDs with names as descriptions; with no daemon → no candidates, no hang. Add a vanilla-zsh check (`autoload -Uz compinit && compinit`, no oh-my-zsh) that `tisvlay <TAB>` completes from the fpath drop-in.
5. `nix build` → confirm `result/share/bash-completion/completions/tisvlay` (and the zsh/fish equivalents) exist and start with the registration.
