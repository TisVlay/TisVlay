# Plans

Working space for in-progress plan docs — anything substantial enough that a multi-commit PR benefits from a checked-in design rationale up front.

## Convention

- One file per plan, `kebab-case.md`.
- Plans live here while the work is active. When the PR merges, migrate the long-term content (decisions worth re-reading, war stories, deferred work pointers) into a durable home in `docs/` and delete the plan file. Git history is the archive.
- Suggested destinations for the typical sections:
  - **Architecture / decisions** → `docs/architecture.md`.
  - **Field-test findings / failure modes** → `docs/lg/` (vendor-specific) or `docs/concerns/` (cross-cutting).
  - **Deferred follow-up work** → `docs/ideas/<topic>.md` with an index entry in `docs/ideas/README.md`.
  - **Process retrospectives / experiment receipts** → fold into the destination doc as a "Lessons" subsection rather than carrying forward as a plan.
- After a plan is migrated, sweep inbound references (`grep -rn 'docs/plans/<name>' --include='*.rs' --include='*.md'`) and re-point them at the new home.

## Active plans

- [`mdbook-docs-site.md`](mdbook-docs-site.md) — user-facing documentation site built with mdBook and published on Read the Docs; CLI reference auto-generated from the clap tree, prose guide covering install, pairing, profiles, session automation, and troubleshooting.
- [`cli-reword.md`](cli-reword.md) — CLI rework: one getter/setter grammar for TV attributes (`--toggle`, `--clear`, on/off values, silent writers), profile catalogs split by scope, per-TV profile assignment verbs, `vendor`/`discover --watch` consolidation, `add` removed; one breaking release, no aliases.
- [`nix-binary-cache.md`](nix-binary-cache.md) — public Cachix cache filled by release-tag CI, with opt-in install docs so NixOS users can skip building from source.
- [`cec-pulse8-autonomous.md`](cec-pulse8-autonomous.md) — disarm the Pulse-Eight adapter's autonomous mode on driver disconnect (a patch in our out-of-tree module build), so an LG TV stops pinning "LG DVD Player" on the PC's input; plus CEC redial when the adapter returns, CEC claim logs and doc corrections.
- [`cec-device-presence.md`](cec-device-presence.md) — local-device presence for CEC: a udev-fed presence fact gates and re-arms dials the way route monitoring does for network TVs, a capped backoff as the safety net, a client-visible "waiting for its device" state, and CEC claim and connect logs.
- [`tv-controls-pc.md`](tv-controls-pc.md) — the reverse direction, all opt-in: the TV powering off locks the session or puts the PC to sleep on any backend, and over CEC the remote's power key and keys act on the PC and the TV can wake it; prerequisites fix the GUI's power-state labels and the sleep handoff, and rename remote sessions to streams.
- [`release-supply-chain.md`](release-supply-chain.md) — verifiable releases: immutable GitHub releases, SLSA Build Level 3 provenance from a reusable build workflow, and per-binary CycloneDX SBOMs checked against cargo-auditable; v0.4.0 waits for it.

When a plan is in flight, list it here with
a one-line summary of the work it covers:

> - [`example-feature.md`](example-feature.md) — short
>   description of what's landing and why a multi-commit PR
>   benefits from an upfront design doc.

Remove the entry when the plan retires and its file deletes
(per Convention above). Git history is the archive — no need
to maintain a retired-plans list here.
