# Plans

Working space for in-progress plan docs — anything substantial enough that a multi-commit PR benefits from a checked-in design rationale up front.

## Convention

- One file per plan, `kebab-case.md`.
- Plans live here while the work is active. When the PR merges, migrate the long-term content (decisions worth re-reading, war stories, deferred work pointers) into a durable home in `docs/` and delete the plan file. Git history is the archive.
- Suggested destinations for the typical sections:
  - **Architecture / decisions** → `docs/design.md`.
  - **Field-test findings / failure modes** → `docs/lg/` (vendor-specific) or `docs/concerns/` (cross-cutting).
  - **Deferred follow-up work** → `docs/ideas/<topic>.md` with an index entry in `docs/ideas.md`.
  - **Process retrospectives / experiment receipts** → fold into the destination doc as a "Lessons" subsection rather than carrying forward as a plan.
- After a plan is migrated, sweep inbound references (`grep -rn 'docs/plans/<name>' --include='*.rs' --include='*.md'`) and re-point them at the new home.

## Active plans

- [`slint-1.17-tooltip.md`](slint-1.17-tooltip.md) — migrate `tisvlay-gui` to a
  git-rev pin of unreleased Slint 1.17, then add a reusable `InfoTip` tooltip
  widget to document pane option controls. Two ordered PRs (migration carries a
  `publish = false` policy change; tooltips need the 1.17 compiler).
- [`tooltip-contextual-help.md`](tooltip-contextual-help.md) — redesign the
  tooltip UX onto KDE conventions: inline helper text + an "i"
  `ContextualHelpButton` toggletip backed by a custom element-anchored popover
  (Slint's built-in `Tooltip` jitters / can't wrap). Supersedes the `InfoTip`
  "?" glyph from the plan above; scoped to the primitive + TVs pane first.

When a plan is in flight, list it here with a one-line summary
of the work it covers:

> - [`example-feature.md`](example-feature.md) — short
>   description of what's landing and why a multi-commit PR
>   benefits from an upfront design doc.

Remove the entry when the plan retires and its file deletes
(per Convention above). Git history is the archive — no need
to maintain a retired-plans list here.
