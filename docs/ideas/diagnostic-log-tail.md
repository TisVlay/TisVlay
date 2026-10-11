# Diagnostic log-tail ring buffer

The About pane's diagnostic blob (schema reference in [`docs/diagnostics.md`](../diagnostics.md)) carries peer + runtime + build metadata after the planned enrichment, but no record of what the GUI was *doing* at capture time. A ring-buffer `tracing-subscriber` `Layer` keeping the last N events drained into the blob would make it trivial to see "what happened in the 30 seconds before this issue."

Implementation sketch:

- New layer in `tisvlay-runtime::logging` (or a sibling module) that writes formatted events into an `Arc<Mutex<VecDeque<String>>>`, capacity-bounded with drop-oldest semantics.
- Wired alongside the existing `fmt::Layer` so console output is unchanged.
- `RUST_LOG` filter governs what the ring sees (single source of truth).
- Default capacity 500 events (~250 KB at typical formatted-event line length); env-var override `TISVLAY_DIAG_BUFFER` for power users.
- Drained at diagnostic-capture time as `Vec<String>` and embedded under `runtime.log_tail`.

Industry convention: Sentry breadcrumbs default to 100 events, dmesg uses byte-bounded rings (16 KB–1 MB), browser DevTools caps at ~10K. 500 is a Sentry-x5 starting point because we paste rather than POST — bigger payload is fine.

Tabled until the PR #1+#2 schema baseline ships so the embedding shape is clear.
