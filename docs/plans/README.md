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

- [`cec-osd-name.md`](cec-osd-name.md) — **implemented**; advertise our own OSD name in the CEC log-address config so the TV labels the PC's HDMI input like a Chromecast; a passive, opt-in self-identification with no capability or client change (vendor-OUI branding kept designed as a deferred, field-gated follow-up). Listed until an on-hardware field test confirms the TV-side relabel.
- [`routable-dials.md`](routable-dials.md) — the connect budget only spends against a network that exists: dials defer while the TV's route is absent, closing the resume/boot carrier race for any NIC cycle time.
- [`wake-screen-primary-input.md`](wake-screen-primary-input.md) — **implemented**; reliable screen elevation (pre-flight wake, root parity), the desired-state latch, and primary-input restoration on automation wakes (CEC selects the requesting port; WoL resumes the last input). Listed until the PR #106 soak confirms it in the field.
- [`gui-state-consistency.md`](gui-state-consistency.md) — **implemented** (matching-gated `caller::Caller`, availability single-sourcing); listed until the PR #106 soak confirms it in the field.

## Completed plans (kept as history)

- [`spike-zenoh-ecs.md`](spike-zenoh-ecs.md) — Zenoh 1.9 + bevy_ecs rewrite exploration: key-space design, ECS world + reactor bridge, milestones M0–M4 with keep/kill criteria. Exited with a keep verdict; continued in the parity plan.
- [`parity-zenoh-ecs.md`](parity-zenoh-ecs.md) — drove the zenoh+ECS stack to full parity and the v0.3.0 switchover; phases P1–P7 with battle-test exits and two live-C3 QA rounds. Durable content distilled into [docs/architecture.md](../architecture.md).

When a plan is in flight, list it here with
a one-line summary of the work it covers:

> - [`example-feature.md`](example-feature.md) — short
>   description of what's landing and why a multi-commit PR
>   benefits from an upfront design doc.

Remove the entry when the plan retires and its file deletes
(per Convention above). Git history is the archive — no need
to maintain a retired-plans list here.
