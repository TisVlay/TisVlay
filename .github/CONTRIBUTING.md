# Contributing to TisVlay

TisVlay controls smart TVs from a Linux desktop. Bug reports, ideas, translations, docs and code are all welcome, and most of the work doesn't need a TV: a mock backend stands in for one.

## Where things go

| You have | Go to |
|---|---|
| A question | [Discussions](https://github.com/TisVlay/TisVlay/discussions) |
| A bug or a feature idea | [The issue forms](https://github.com/TisVlay/TisVlay/issues/new/choose) |
| A security problem | [The security policy](SECURITY.md); please don't open a public issue |
| A translation | [docs/i18n.md](../docs/i18n.md) |
| A new TV backend | [docs/backends/implementing-a-vendor.md](../docs/backends/implementing-a-vendor.md) |
| A change bigger than a small fix | An issue first, so the approach is agreed before you plan it ([how changes land](#how-changes-land)) |

## Setup

```bash
git clone https://github.com/TisVlay/TisVlay.git
cd TisVlay
nix develop     # or `direnv allow`
prek install    # the pre-commit and commit-msg hooks
```

The [Nix](https://nixos.org/download/) dev shell provides the Rust toolchain, `just` and every tool the checks use. Without Nix, [the CI workflow](workflows/ci.yml) shows what a plain Ubuntu machine needs. Images under `docs/` are stored in Git LFS, so install it if you're working on them.

Run each service in its own terminal:

```bash
just daemon-start
just session-start
just tray-start
just gui-start
```

In VS Code, the `all: start`, `all: stop` and `all: restart` tasks do this for you, with a panel per service. They need the dev shell's environment to find `just`, so start VS Code from the dev shell or use a direnv extension.

Then add the mock TV: click **+** on the **TVs** page, choose **Don't see your TV? Add it manually**, pick the vendor **Mock (testing)** and enter an address such as `mock://test`. `just cli <verb>` runs any CLI verb against the same stack, and `just stop-all` ends it.

The dev stack uses its own sockets, so it won't disturb an installed TisVlay.

Run `just` to list every recipe.

## How changes land

Small fixes go straight to a PR. Anything substantial (several commits, a wire or key change, work across crates) follows a plan:

1. **Plan.** Draft `docs/plans/<name>.md`: the goal, the decisions and why, commit-sized steps, and how each step will be verified. Commit it and open a draft PR, so later changes show up as diffs.
2. **Review the plan** for design soundness, implementation correctness (do its claims about the code hold?) and maintainability. An agent or a person can do this, and the maintainer settles contested decisions.
3. **Implement,** one commit per step, each passing `just check`, with tests in the same commit as the code. A bug fix starts with a test that reproduces it, where one can. When the work departs from the plan, record what changed and why in the plan, in the same commit.
4. **Review the implementation** with the same lenses, and test on hardware whatever the tests can't reach. Fixes land as new commits.
5. **Retire the plan** in the last commit, moving what's worth keeping into `docs/` ([how](../docs/plans/README.md)).

## Before opening a PR

- Run `just check`, which covers most of what CI gates; CI also checks the minimum Rust version, feature combinations and the book ([what runs when](../docs/build.md#what-runs-when)). For a docs-only change, `prek run -a --skip build --skip clippy --skip test` is enough.
- If you changed the CLI or a profile default, run `just docs-gen`. If you changed a user-facing GUI string, run `just i18n-extract`. Commit what they regenerate; CI fails on stale generated docs.
- Add tests for what you changed. [docs/testing.md](../docs/testing.md) covers the layers and where new tests go.

## Commits and PRs

- **The PR title must follow [Conventional Commits](https://www.conventionalcommits.org/)**, for example `fix(lg): reconnect after a standby`, and CI checks it.
- **Small PRs are squash-merged,** so the title becomes the commit subject on `main`, which sets the next version and the changelog entry. Commits on the branch don't need to be tidy.
- **Plan-driven PRs keep their commits** through a merge commit, so the plan and its history stay in git. Each commit's subject then feeds the version and the changelog, so every one must follow Conventional Commits. The commit-msg hook checks them.
- **Draft PRs** run only the fast checks; marking one ready starts the full run ([what runs when](../docs/build.md#what-runs-when)).
- **Say what you tested on:** which TV and backend, or the mock.
- **AI-assisted work is welcome and held to the same standard:** you understand what you submit, you've tested it, and you can answer questions about it. It doesn't need to be disclosed.

## Code conventions

These are the rules the lints can't fully express. The lint policy and its reasons are in [docs/build.md § Lint policy](../docs/build.md#lint-policy).

- **Panics state their invariant.** `expect("… because …")`, never a bare `unwrap()` in shipped code. Tests may unwrap.
- **Errors:** published crates and the client SDK expose `thiserror` types callers can match on. Binaries and binary-glue libraries (`tisvlay-runtime`, the service crates' library halves) may use `anyhow`. The model is in [docs/architecture.md § Error model](../docs/architecture.md#error-model).
- **Logging:** through `tracing`, with structured fields (`info!(%tv, "connected")`) rather than values formatted into the message. No `println!`/`eprintln!` outside the CLI (lint-enforced).
- **Secrets never reach `Debug`.** A typed secret is a `tisvlay_core::Secret`: its `Debug` prints `[REDACTED]`, serde sees a plain string, and the value is read through `expose()`. The LG client key is one everywhere it's typed (`Credentials`, `LgVendorConfig`, the register payloads, `WebOsClient`); a new credential field uses it too. The untyped vendor config is a `tisvlay_core::VendorConfig`, and the wire requests that carry one (`AddTv`, `UpdateConfig`) are hand-written the same way: their `Debug` prints key names only. `Secret` wraps secrecy 0.10 (redacted `Debug`, zeroize on drop, `Serialize` opt-in); zenoh-config still pins 0.8, so `deny.toml` skips that duplicate.
- **Handles whose drop cancels work** (watch handles, the service stacks, liveliness guards) carry `#[must_use = "…"]` saying what dropping them does.
- **Published crates** (core, protocol, and the `tisvlay` SDK) keep items private by default. `#[non_exhaustive]`, public-dependency and README rules are in [docs/build.md § Public API policy](../docs/build.md#public-api-policy); `just package-check` packages them.
- **A wire-visible change is a `feat` or a breaking commit, never a `fix`:** any change to a key, payload, rpc or decode rule. Clients and services check each other's release on `major.minor`, and release-plz bumps the minor only for those commit types, so a protocol change shipped as a patch would pass the check and fail at runtime.
- **A new rpc is declared once** in its family's table in `tisvlay-protocol`'s `endpoint` module (key, request, response, read or write); the compiler then demands the server arm. A world-answered rpc also gets a `Command` variant, and the reactor's exhaustive match then demands its one-shot handler in `tisvlay-world`'s `systems/commands.rs`. Add the SDK or CLI surface by hand. Requests decode strictly: no compatibility shims on the wire.
- **A new per-TV attribute in the CLI** is a getter/setter verb, `<attribute> <tv> [<value>]`, with `--toggle`/`--clear` where they apply: no `set-` prefix, `on|off` for switches, and the bare form reads the value. No aliases for renamed verbs.
- **Allowances:** reasoned `#[expect(lint, reason = "…")]`. Use `#[allow]` with a reason only where the lint depends on features or tests.
- **`unsafe`:** only in files that carry a reasoned `#![expect(unsafe_code)]`, one operation per block, each with a `// SAFETY:` comment.
- **Spelling and wrapping:** American English, which the typos gate enforces. Markdown isn't hard-wrapped: one paragraph per line.

## Docs

The user guide is the mdBook in [docs/book/](../docs/book/), published on [Read the Docs](https://tisvlay.readthedocs.io/). Everything else under `docs/` is for developers.

- **One canonical home per fact.** Elsewhere, keep only the consequence the reader needs at that point, in a clause, plus a link.
- **Values that live in code are generated,** into `docs/book/src/generated/` behind a drift gate. Don't hand-copy a default or a flag into the prose.
- **Describe tasks with the labels the GUI shows,** taken from `crates/tisvlay-gui/ui/`, and keep implementation vocabulary (UUIDs, sockets, field names) off the main path. When a label changes, grep the book for the old one.
- **Command-line equivalents live in one of two places:** the "Which command does what the app does" table on the Command Line page, or a collapsed "From a terminal" `<details>` aside where a terminal really is the fix.
- **Stay agnostic to vendor and transport.** Vendor specifics go in a per-backend table or an explicit "for LG" clause.

Canonical homes in the book:

| Fact | Page |
|---|---|
| Profile semantics, inheritance, the automation gate, event meanings | Automation |
| The file schema and reload rules | Configuration Files |
| Stream behavior | Streaming |
| Tray and window behavior | GUI & Tray |
| The Wake-on-LAN checklist | Troubleshooting |
| The MAC explanation | Adding a TV |
| Checking a release (checksums, signatures, SBOMs, scanning an installed system) | Security & Supply Chain |

## Where to read more

- [docs/architecture.md](../docs/architecture.md): topology, key space, the world and reactor model, and what's been learned about zenoh.
- [docs/build.md](../docs/build.md): the build, lint policy, public API policy and what CI runs.
- [docs/testing.md](../docs/testing.md): test layers, the mock backend and the link-breaking relay.
- [docs/ideas/](../docs/ideas/README.md), [docs/concerns/](../docs/concerns/README.md) and [docs/bugs/](../docs/bugs/README.md): deferred work, design concerns and known bugs, each with an index. Good places to find something to pick up.

## License

TisVlay is licensed under [Apache-2.0](../LICENSE). Under section 5 of the license, anything you submit for inclusion is under the same terms, unless you explicitly state otherwise. There's no CLA or sign-off.
