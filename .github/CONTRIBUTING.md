# Contributing to TisVlay

TisVlay controls smart TVs from a Linux desktop. Bug reports, ideas, translations, docs and code are all welcome, and most of the work doesn't need a TV: a mock backend stands in for one.

## Where things go

| You have | Go to |
|---|---|
| A question | [Discussions](https://github.com/TisVlay/TisVlay/discussions) |
| A bug or a feature idea | [The issue forms](https://github.com/TisVlay/TisVlay/issues/new/choose) |
| A security problem | [The security policy](SECURITY.md); please don't open a public issue |
| A translation | [docs/i18n.md](../docs/i18n.md) |
| A new TV backend | An issue first, then [docs/backends/implementing-a-vendor.md](../docs/backends/implementing-a-vendor.md) |
| A change bigger than a small fix | An issue first, so the approach is agreed before you plan it ([how changes land](#how-changes-land)) |

## Setup

```bash
git clone https://github.com/TisVlay/TisVlay.git
cd TisVlay
nix develop     # or `direnv allow`
prek install    # the pre-commit and commit-msg hooks
```

The [Nix](https://nixos.org/download/) dev shell provides the Rust toolchain, `just` and every tool the checks use. Without Nix, [the CI workflow](workflows/ci.yml) shows what CI installs on Ubuntu, which is a starting point. Images under `docs/` are stored in Git LFS, so install it if you're working on them.

`prek install` makes every commit run the full build, clippy and the tests. For just the commit subject check, use `prek install -t commit-msg` and run the checks yourself before pushing.

Run each service in its own terminal, each inside the dev shell:

```bash
just daemon-start
just session-start
just tray-start
GUI_LIVE=false just gui-start
```

`just gui-start` defaults to Slint's live preview, which can crash when a click navigates away from what was clicked, so `GUI_LIVE=false` turns it off. In VS Code, the `all: start`, `all: stop` and `all: restart` tasks run the stack with a panel per service, with live preview on. They need the dev shell's environment to find `just`, so start VS Code from the dev shell or use a direnv extension.

Then add a mock TV, as [Troubleshooting](../docs/book/src/operations/troubleshooting.md#isolating-tisvlay-from-the-tv-mock-backend) describes. `just cli <verb>` runs any CLI verb against the same stack, and `just stop-all` ends it. Run `just` to list every recipe.

If TisVlay is also installed on the machine, only the daemon has its own socket and state. The dev session and tray share the installed ones' user socket and config files, and the installed GUI and CLI connect to a running dev daemon. So first stop the installed user services (`systemctl --user stop tisvlay-session tisvlay-tray`) and close its GUI, and run with `EPHEMERAL=true` so the dev session doesn't save to your config.

## How changes land

Small fixes go straight to a PR. Anything that changes a wire key, payload or rpc, adds a feature, or needs reviewing in stages follows a plan. If you're unsure, ask on the issue; the maintainer decides. If a PR outgrows "small", or you've already written the code, open it anyway and say so: the maintainer will take it as is or ask for a short plan on the same branch.

1. **Agree the approach** in an issue.
2. **Plan.** Draft `docs/plans/<name>.md` ([what it holds](../docs/plans/README.md#what-a-plan-holds)). Commit it and open a draft PR, so later changes show up as diffs.
3. **Get the plan reviewed.** Ask for review on the draft PR and wait for the maintainer's agreement before implementing. Reviewing it yourself or with an agent first is welcome. A review looks at design soundness, implementation correctness (do the plan's claims about the code hold?) and maintainability.
4. **Implement,** one commit per step, each passing the checks, with tests in the same commit as the code. A bug fix starts with a test that reproduces it, where one can. When the work departs from the plan, record what changed and why in the plan, in the same commit. Mark the PR ready when you want the full CI run.
5. **Get the implementation reviewed** with the same lenses. Fixes land as new commits. Whatever the tests can't reach gets tested on hardware; if you don't have it, say what's untested, and the maintainer will test it or find someone who can.
6. **Retire the plan** when the maintainer says the PR is ready to merge, moving what's worth keeping into `docs/` ([how](../docs/plans/README.md#convention)). If you're unsure what to keep, ask.

## Before opening a PR

- Run `prek run -a`. CI also checks the minimum Rust version, feature combinations, coverage and the book ([what runs when](../docs/build.md#what-runs-when)). For a docs-only change, `prek run -a --skip build --skip clippy --skip test --skip doc` is enough, plus `just docs-links` if you touched the book.
- If you changed the CLI or the profile structs (defaults, fields or their doc comments), run `just docs-gen`. If you changed a user-facing GUI string, run `just i18n-extract`. Commit what they regenerate; CI fails on stale generated docs.
- Add tests for what you changed, where it applies. [docs/testing.md](../docs/testing.md) covers the layers and where new tests go.

## Commits and PRs

- **The PR title must follow [Conventional Commits](https://www.conventionalcommits.org/),** for example `fix(lg): reconnect after a standby`, and CI checks it. release-plz derives the next version and the changelog from the subjects on `main`, for commits that touch a published crate.
- **Small PRs are squash-merged** under that title, so commits on the branch don't need to be tidy.
- **Plan-driven PRs keep their commits** through a merge commit, so the plan and its history stay in git. Every commit subject on the branch must follow Conventional Commits too, and CI checks that once a commit touches `docs/plans/`. To fix a bad subject, reword it and force-push your own branch before merge, the one exception to fixes landing as new commits. To sync with `main`, merge it in.
- **Draft PRs** run only the fast checks; marking one ready starts the full run ([what runs when](../docs/build.md#what-runs-when)). On your first PR, CI waits for the maintainer to approve the run.
- **Say what you tested on:** which TV and backend, or the mock.
- **AI-assisted work is welcome and held to the same standard:** you understand what you submit, you've tested it, and you can answer questions about it. It doesn't need to be disclosed.

## Where to read more

- [docs/conventions.md](../docs/conventions.md): the code conventions the lints can't express. Read this before writing code.
- [docs/book/README.md](../docs/book/README.md): the rules for writing the user guide.
- [docs/architecture.md](../docs/architecture.md): topology, key space, the world and reactor model, and what's been learned about zenoh.
- [docs/build.md](../docs/build.md): the build, lint policy, public API policy and what CI runs.
- [docs/testing.md](../docs/testing.md): test layers, the mock backend and the link-breaking relay.
- [docs/ideas/](../docs/ideas/README.md), [docs/concerns/](../docs/concerns/README.md) and [docs/bugs/](../docs/bugs/README.md): deferred work, design concerns and known bugs, each with an index. Good places to find something to pick up.

## License

TisVlay is licensed under [Apache-2.0](../LICENSE). Under section 5 of the license, anything you submit for inclusion is under the same terms, unless you explicitly state otherwise. There's no CLA or sign-off.
