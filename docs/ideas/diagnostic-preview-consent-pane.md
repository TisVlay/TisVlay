# Diagnostic preview-and-consent pane

Today's "Diagnostics" button copies the blob immediately. Once the blob grows beyond the current 4-field stub (peers + runtime + tvs + log tail), the user should be able to *see* what's about to leave their machine and toggle off categories they'd rather not share.

Path forward:

- Modal sheet over the About pane (translucent overlay, dismissable). Discoverable; doesn't fight existing pane navigation.
- Scrollable JSON preview rendered via the same browser-handoff path the third-party-notices view uses, or an in-pane code-style read-only text widget — TBD pending a Slint perf check on long JSON.
- Category toggle chips: System info, Log tail, Vendor data, Build provenance. Each gates a top-level section in the captured payload.
- Three action buttons in the sheet footer: Copy, Save File, Open Issue (URL-prefill via `https://github.com/.../issues/new?body=<urlencoded>`, with truncation when over GitHub's URL cap).

Builds on the support-bundle CLI work below — both share the same `Diagnostic::redact_categories(&[Category])` helper.

