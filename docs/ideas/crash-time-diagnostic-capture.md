# Crash-time diagnostic capture

When the GUI panics, the blob the user could have copied is gone. Wire a panic hook that writes the diagnostic blob to `~/.cache/tisvlay/crash-<ISO8601>.json` so post-crash triage doesn't depend on the user remembering to copy first.

Stack:

- `human-panic` for the friendly user-facing panic message + path-to-report shown on stderr.
- Custom panic hook that calls `Diagnostic::capture()` (synchronous; the panicking thread writes the file before unwind) and chains into `human-panic`.
- `tracing-error` for `SpanTrace` capture into errors at the daemon and GUI's error boundaries — gives the report a "what was happening" stack alongside the Rust backtrace.
- `color-eyre` (or `eyre` directly) at error-bubbling boundaries so non-panic errors that surface to the user get the same hook treatment.

Trade-off: `human-panic` writes to a temp dir by default; we override the path to `~/.cache/tisvlay/` so users have a stable place to find it. Adds three runtime crates (`human-panic`, `tracing-error`, `color-eyre`); none are heavy.

