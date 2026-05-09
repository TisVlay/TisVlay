# Consolidate `tisvlayd` integration test binaries

Today `tisvlayd` has 12 integration test files under `crates/tisvlayd/tests/` (`pair_round_trip`, `profile_dispatch`, `update_config_round_trip`, `remove_tv_round_trip`, `backlight_emit_ordering`, `reload_resolves_system_profile`, `scroll_apply_under_ramp`, `automated_gate`, `concurrent_tvs`, `shutdown_cancel`, `test_harness_smoke`, `dbus_contract`). Each is its own integration-test binary, so cargo links 12 separate ELFs every time the test surface is touched.

The build-time work measured this. On the local 32-core box, all 12 link in parallel — start at t≈48.85 s, finish by t≈53.4 s — so the whole batch costs ~5 s wall. But each binary is 4–5 s of CPU, so 12 × 4.5 ≈ 54 s of CPU work that on a 2-core CI runner would serialize into ~25 s wall. See [`docs/build.md`](../build.md) for the broader build-system context.

**Estimated CI saving from consolidating into ~4 logical groups:** 5–10 s of `just test-ci` wall time. **Estimated local saving:** 0–2 s. Effort: medium (group tests by topic, update imports, rename test functions to keep `nextest` filter strings sensible). Risk: medium — process-per-test isolation under nextest is preserved (test isolation isn't lost), but a poorly-grouped consolidation can make a local test failure harder to triage when twelve unrelated tests share one binary.

## Trigger condition

Open this when CI cycle time on `just test-ci` becomes a real bottleneck for PR turnaround, **or** when sysroot constraints push the runner closer to disk/memory limits and we need to shed link footprint. Until then the data says the local pain isn't real and the CI saving doesn't justify the refactor.

## Suggested grouping

A first cut on logical groups, by D-Bus surface area:

- **`tv_lifecycle`** — `pair_round_trip`, `remove_tv_round_trip`, `update_config_round_trip`, `concurrent_tvs`
- **`profile_and_config`** — `profile_dispatch`, `reload_resolves_system_profile`, `automated_gate`
- **`runtime_signals`** — `backlight_emit_ordering`, `scroll_apply_under_ramp`, `shutdown_cancel`
- **`contract_smoke`** — `dbus_contract`, `test_harness_smoke`

Adjust based on which tests share helpers; a few of these import the same `TestDaemon` setup, so grouping by setup cost might beat grouping by topic.

## What this is *not*

Not a way to reduce test count or coverage — every test stays. Not a fix for slow individual tests — those want their own profiling. Not applicable to `tisvlay-session` / `tisvlay-gui` / `tisvlay-cli` test surfaces; they each have far fewer integration test files (≤4) and the parallelism story already works for them.
