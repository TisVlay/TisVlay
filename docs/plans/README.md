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

- [`tooltip-contextual-help.md`](tooltip-contextual-help.md) — **(as-built / merged)**
  KDE-style contextual help: an "i" `ContextualHelpButton` toggletip + hover tips
  on icon buttons / links / chips, drawn by an in-window overlay (`Help` global +
  `HelpOverlay`) with focus-out dismiss, shrink-to-fit width, anchor tracking, and
  ScrollView-clamped placement; rolled out across all panes. Consolidates the
  earlier Slint 1.17 migration plan and folds in the 1.17.0 outcome (incl. the
  upstream `system-tray` fix) plus the native `Tooltip`/`PopupWindow` evolution
  notes worth upstreaming.

When a plan is in flight, list it here with a one-line summary
of the work it covers:

> - [`example-feature.md`](example-feature.md) — short
>   description of what's landing and why a multi-commit PR
>   benefits from an upfront design doc.

Remove the entry when the plan retires and its file deletes
(per Convention above). Git history is the archive — no need
to maintain a retired-plans list here.
