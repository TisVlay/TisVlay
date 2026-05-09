# Opt-in remote crash reporting (`dev-telemetry` feature)

Once the diagnostic blob and crash-time capture are in place, the natural next step is shipping reports to a backend instead of asking users to upload files manually. This is gated work — production binaries should not depend on remote ingestion.

Approach:

- New Cargo feature `dev-telemetry` on `tisvlay-gui`, default off. Enables `sentry-rust` (panic integration + tracing layer + breadcrumbs + release tags).
- `SENTRY_DSN` env-var-gated: even with the feature on, no DSN means no transmission.
- Backend choices, all DSN-compatible (no SDK swap):
  - **GlitchTip** — FOSS, MIT, single Django container + Postgres + Redis (~512 MB RAM). v6.1 (March 2026) added minidump support.
  - **Bugsink** — FOSS, MIT, single container + SQLite. $5 VPS-friendly.
  - **Sentry SaaS** — free tier 5K errors/month; hosted Sentry self-host is heavy (16 GB RAM, ~25 services).
- Recommended exploration path: local GlitchTip via Docker Compose first; document the spin-up in `docs/observability.md`. DSN flip moves the same code to Bugsink or hosted Sentry without changes.

Production release builds compile out the feature entirely — `sentry-rust` is not in their dep graph at all.

Conscious skips: PostHog (product analytics first; error tracking secondary; heavier infra), Highlight.io (acquired/shutdown 2026-02-28), Apache SkyWalking (APM/distributed tracing, not panic grouping), Bugsnag/Rollbar/Honeybadger (proprietary SaaS, no compelling Rust SDK over `sentry-rust`).

