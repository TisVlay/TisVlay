# Plans

Working space for in-progress plan docs — anything substantial enough that a multi-commit PR benefits from a checked-in design rationale up front. The steps from plan to merge are in the [contributing guide](../../.github/CONTRIBUTING.md#how-changes-land).

## What a plan holds

- The goal, and why it's worth doing.
- The decisions, each with its reason.
- Commit-sized steps, and how each will be verified: by tests, or on hardware where tests can't reach.
- Deviations, added as the work goes: what changed from the plan and why, in the commit that departs from it.

## Convention

- One file per plan, `kebab-case.md`.
- Plans live here while the work is active. In the PR's last commit, migrate the long-term content (decisions worth re-reading, war stories, deferred work pointers) into a durable home in `docs/` and delete the plan file. Plan-driven PRs keep their commits through a merge commit, so git history is the archive.
- Suggested destinations for the typical sections:
  - **Architecture / decisions** → `docs/architecture.md`.
  - **Field-test findings / failure modes** → `docs/lg/` (vendor-specific) or `docs/concerns/` (cross-cutting).
  - **Deferred follow-up work** → `docs/ideas/<topic>.md` with an index entry in `docs/ideas/README.md`.
  - **Process retrospectives / experiment receipts** → fold into the destination doc as a "Lessons" subsection rather than carrying forward as a plan.
- After a plan is migrated, sweep inbound references (`grep -rn 'docs/plans/<name>' --include='*.rs' --include='*.md'`) and re-point them at the new home.

## Active plans

- [`contributing.md`](contributing.md) — a contributing guide and issue and PR templates under `.github/`, with the code conventions moved out of `CLAUDE.md` and the mdBook plan retired.

When a plan is in flight, list it here with a one-line summary of the work it covers:

> - [`example-feature.md`](example-feature.md) — short description of what's landing and why a multi-commit PR benefits from an upfront design doc.

Remove the entry when the plan retires and its file deletes (per Convention above). Git history is the archive — no need to maintain a retired-plans list here.
