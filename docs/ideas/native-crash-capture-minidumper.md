# Native crash capture via `minidumper`

`sentry-rust`'s panic integration captures Rust panics. Native crashes (SIGSEGV in Slint's GPU / wgpu / Skia path, third-party native deps) bypass it — those are the bugs that most need crash reports because the user has nothing to copy from a frozen UI.

Stack:

- Embark's `crash-handler` crate — installs signal handlers for SIGSEGV / SIGBUS / SIGFPE / SIGILL / SIGABRT.
- Embark's `minidumper` — child-process minidump writer; the parent's signal handler hands off to the child so the dying parent doesn't have to allocate.
- Upload via the Sentry minidump endpoint (GlitchTip 6.1+ supports it; Sentry SaaS does too).

Gated under the same `dev-telemetry` Cargo feature as the `sentry-rust` integration. Minidump symbolication needs debug symbols on the receiving end; for self-hosted GlitchTip, ship a separate build artifact (`*.dwp` or `*-symbols.zip`) and upload via `sentry-cli upload-difs` or GlitchTip's `dif` API.

Trade-off: the most heavyweight piece of the diagnostic stack. Worth it only when Slint/GPU crashes start showing up as recurring user reports.
