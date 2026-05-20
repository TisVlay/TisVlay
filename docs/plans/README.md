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

- [profile-crud-v3.md](profile-crud-v3.md) — third attempt at landing profile management. Resets to a property-bag wire surface (`Manager1.UpdateSystemProfile(name, a{sv})` etc.) after v2's per-property typed-property pattern over-corrected for v1's GUI-side latency problem. Carries every UI polish + widget + dialog primitive v2 contributed; drops the per-profile D-Bus object infrastructure. Unblocked by zvariant 5.11+'s nested `DeserializeDict` support.
