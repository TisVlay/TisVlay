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

> - [`cec-backend.md`](cec-backend.md) — a new `tisvlay-cec` vendor
>   backend controlling TVs over HDMI-CEC (`/dev/cecN`) via Valve's
>   `linux-cec` library; v1 scope is power + active-source input,
>   built software-first against a fakeable seam since no real
>   CEC link exists on the dev box yet.
> - [`cec-screen-via-power.md`](cec-screen-via-power.md) — a per-device
>   opt-out CEC config (`emulate_screen_via_power`, default on) that
>   fulfils screen-blank profile actions by powering the TV, so the
>   shipped default profiles behave consistently on a CEC-only setup.

When a plan is in flight, list it here with a one-line summary
of the work it covers:

> - [`example-feature.md`](example-feature.md) — short
>   description of what's landing and why a multi-commit PR
>   benefits from an upfront design doc.

Remove the entry when the plan retires and its file deletes
(per Convention above). Git history is the archive — no need
to maintain a retired-plans list here.
