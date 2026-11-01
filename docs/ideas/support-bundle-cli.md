# Support-bundle CLI subcommand (`tisvlay diagnostic`)

Mirrors `kubectl support-bundle` / `docker support-bundle`. Single CLI entrypoint that emits the diagnostic JSON to stdout or zips a richer bundle.

Surfaces:

- `tisvlay diagnostic` — JSON to stdout, suitable for `| pbcopy`, `| jq`, or piping into a gist.
- `tisvlay diagnostic --bundle <path>` — zip containing `diagnostic.json` + `tisvlay.log` (recent journalctl extract or the ring-buffer tail) + `daemon-config.toml` (redacted) + `tvs.json` (a one-shot `tisvlay --json list` snapshot).

New deps: `zip` for archive creation. Logs path: `journalctl --user-unit tisvlay-* -n 1000 -o json` if the ring-buffer subscriber hasn't shipped, otherwise the in-memory tail.

GUI integration: the consent-pane "Save File" action calls the same `Diagnostic::write_bundle(path)` helper the CLI uses, ensuring the artifacts users paste and the artifacts maintainers pull match.
