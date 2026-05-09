# Re-enable Slint `accessibility` feature once upstream blockers clear

`tisvlay-gui` ships with the slint `accessibility` Cargo feature off (rationale lives in the Cargo.toml comment block). The original blocker on our side was a runtime conflict — `accesskit_unix` spawns its own thread that uses zbus, and zbus's tokio backend panics with "no reactor running" in that thread because it's not part of our tokio runtime. That symptom isn't filed upstream; the work was deferred after surfacing three independent upstream blockers that need to clear first.

Gating conditions before this work is worth picking up:

- **[slint-ui/slint#11663](https://github.com/slint-ui/slint/pull/11663)** — winit 0.31 port temporarily disables accesskit because it requires upstream changes. Until this lands, anything we figure out will likely need reshaping. Track until merged.
- **[slint-ui/slint#10269](https://github.com/slint-ui/slint/issues/10269)** — panic in `accesskit_consumer` when `LineEdit` / `Slider` is clicked after a state mutation driven by `upgrade_in_event_loop`. `tisvlay-gui` runs that pattern continuously (Daemon-event → Slint property update → user clicks), so re-enabling the feature today would expose this independent of any zbus-runtime fix on our side.
- **[slint-ui/slint#3867](https://github.com/slint-ui/slint/issues/3867)** — `i_slint_backend_winit::accesskit` is flamegraph-dominant on large `ListView`s. Less of a blocker than a "feature off in production until needed" reinforcer.

Starter task whenever this is picked up: file an upstream issue against `accesskit_unix` (or slint, depending on responsibility) with a minimal repro of the zbus-tokio "no reactor" panic. Even if our local fix lands first, surfacing the symptom upstream helps the next user hitting it.

Investigation paths to consider once the upstream picture is clearer:

- **Path A: configure zbus to share our tokio runtime** — the `tokio::runtime::Handle::try_current()` route, possibly wrapping `accesskit_unix`'s connection setup with `tokio::task::block_in_place` or by handing it a pre-built `zbus::Connection` from our runtime.
- **Path B: switch zbus to async-io for accesskit only** — zbus has feature-flag selection between tokio and async-io. Whether two runtimes can coexist in one process is the open question.

Independent of all of the above, our test infrastructure does **not** depend on this — Slint's testing backend reads `accessible-*` properties via the internal element tree, not via AT-SPI, so the shipped `accessible-label` / `accessible-id` surface (see [`docs/testing.md`](../testing.md)'s "GUI testing" section) works regardless of the production feature state.
