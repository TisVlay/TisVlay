# Contributing guide

Give outside contributors a front door: a short guide that says where each kind of contribution goes and what a mergeable PR needs, and links to the existing docs for depth. GitHub links the file whenever someone opens an issue or PR. So far every PR has come from the maintainer or Dependabot, so the guide is written for a first-time outsider and stays small.

## Decisions

| Topic | Decision |
|---|---|
| Location | Everything GitHub reads from `.github/` lives there, so the root checkout stays clean. `README.md` and `LICENSE` stay at the root (see Layout). |
| Code conventions | Move from `.claude/CLAUDE.md` § Code conventions into the guide. `CLAUDE.md` imports the guide with `@../.github/CONTRIBUTING.md`, so there's one copy and Claude still loads it each session. |
| Merge policy | Outside PRs are squash-merged, so contributors only need a Conventional Commits PR title. Maintainer trains keep merge commits. |
| Dev setup | Lead with the Nix dev shell. For a non-Nix setup, point to the CI workflow rather than writing steps that can drift. |
| Licensing | Apache-2.0 §5 covers contributions. No CLA, no DCO sign-off. |
| Conduct | Nothing for now: no `CODE_OF_CONDUCT.md`, no expectations section. |
| AI-assisted PRs | Held to the same standard: the author understands and tests what they submit. No disclosure required. |
| Companion files | PR template, bug-report form, feature-request form. |
| Discussions | Enable them, and send questions there instead of to issues. |
| mdBook plan | Retire it in this PR. Read the Docs is live. Its authoring rules move into the guide. |

## Layout

Moves into or lands in `.github/`:

- `CONTRIBUTING.md` (new)
- `SECURITY.md` (moved from the root)
- `pull_request_template.md` (new)
- `ISSUE_TEMPLATE/bug_report.yml`, `feature_request.yml`, `config.yml` (new)

Already there: `dependabot.yml`, `workflows/`.

Stays at the root:

- **`LICENSE`:** GitHub's docs say license files are included "when a project is cloned, packaged, or downloaded" and describe the root as the usual place. The root is also where people and license scanners look.
- **`README.md`:** GitHub would surface a `.github/README.md`, but anyone reading a clone or a tarball expects it at the root, and its relative links would all change.

GitHub's page on contributor guidelines states `.github/` for `CONTRIBUTING.md`, and its PR-template page states it for `pull_request_template.md`. Issue forms only ever live in `.github/ISSUE_TEMPLATE/`. The pages read didn't state a location for a repository's `SECURITY.md`, so check that one after merge (see Verification).

## Guide outline

1. **Ways to help, and where each goes:** bugs and feature ideas (issue forms), questions (Discussions), translations (`docs/i18n.md`), new backends (`docs/backends/implementing-a-vendor.md`), security reports (`SECURITY.md`).
2. **Setup:** the Nix dev shell, `prek install`, and running the stack against the mock backend, so no TV is needed (`docs/testing.md` § Running the mock in development).
3. **Before opening a PR:** `just check`, and the regeneration steps the gates enforce (`just docs-gen`, `just i18n-extract`).
4. **Commits and PRs:** a Conventional Commits PR title, which becomes the subject on `main` and drives release-plz. A wire-visible change is a `feat` or breaking, never a `fix`. AI-assisted work meets the same standard.
5. **Code conventions:** moved from `CLAUDE.md`, linking to `docs/build.md` § Lint policy for the lint rules and their reasons.
6. **Docs:** the book's authoring rules, from the mdBook plan's GUI-first section: GUI labels on the main path, one canonical home per fact, and a clause and a link everywhere else.
7. **Where to read more:** `docs/architecture.md`, `docs/build.md`, `docs/testing.md`, the `docs/ideas/`, `docs/concerns/` and `docs/bugs/` indexes.
8. **License:** one sentence on Apache-2.0 §5.

`CLAUDE.md` imports the whole guide, so every line also costs context in each Claude session: another reason to keep it short.

## Companion files

- **PR template:** what and why, checks run, partials or translations regenerated, wire-visible change flagged.
- **Bug-report form:** version, install method, TV model, backend, desktop, what happened, what was expected, and logs. Point to the book's Troubleshooting page for gathering logs rather than copying the commands. Label: `bug`.
- **Feature-request form:** the problem, the proposed behavior, alternatives considered. Label: `enhancement`.
- **`config.yml`:** blank issues off, with contact links to Discussions, the book and the security advisory form.

## Other changes

- **CI classifier:** `.github/workflows/ci.yml`'s `docs` pattern names `SECURITY\.md$`. Replace it with the guide, policy and templates under `.github/`, without matching `workflows/` or `dependabot.yml`, or a change to the guide would run the full build.
- **`docs/build.md`:** update § What runs when's list of docs-only paths. Its opening also calls `design.md` the home of architectural decisions, but that doc covers the retired stack, so point it at `architecture.md`.
- **`pr-title.yml`:** its comment says the repo squash-merges. Reword it to the policy above. Grep for the same claim elsewhere.
- **`README.md`:** add a contributing link to § Documentation.
- **Book:** add a contributing link beside the FAQ's bug-report pointer.
- **`CLAUDE.md`:** add the import, remove § Code conventions, list the new files in the project structure, and drop the What's Next "User docs" entry.

## Retiring the mdBook plan

- Authoring rules go to the guide's Docs section.
- The deferred generators (G3 NixOS options page, G4 vendor tables) go to a `docs/ideas/` entry with an index line.
- Check whether the deferred GUI and tray screenshots are still missing from the book. If they are, add them to that same idea entry.
- Sweep the inbound references: `CLAUDE.md`'s What's Next, `docs/plans/README.md`, and the comment in `flake.nix`.

## Commits

One PR on `docs/contributing-guide`:

1. This plan, as the baseline.
2. The guide, the `SECURITY.md` move, the conventions move, and the CI and docs updates that depend on them.
3. The PR template and issue forms.
4. Retire the mdBook plan.
5. Retire this plan, last, once review feedback is in.

## Verification

Before merge:

- `prek run -a` and `just docs-links`.
- Run the new classifier pattern against sample paths: the guide and templates count as docs-only, while `.github/workflows/ci.yml`, `.github/dependabot.yml` and a crate README don't.
- Follow the guide's setup steps from a fresh clone, including the mock-backend run.
- Read the rendered guide and issue forms on the branch.

Enable Discussions just before merge, so the new links don't point at a page that doesn't exist yet. It's a repository setting, so confirm with the maintainer first.

After merge, since GitHub reads these from the default branch:

- `gh api repos/TisVlay/TisVlay/community/profile` reports the contributing guide, PR template and issue templates.
- The Security tab still shows the policy from `.github/SECURITY.md`. If it doesn't, move the file back to the root.
- The relative links in the guide work from both the Contributing tab and the file view. If the tab breaks `../docs/…` links, use root-relative `/docs/…`, which GitHub's docs say resolve from the repository root.
- The new-issue page shows both forms and the contact links.
