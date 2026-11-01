# Intermittent test failures

**Priority:** low. **Found:** 2026-10-24 and 2026-10-31, under full-suite load.

## Observations

- **`tisvlay-session` `unarmed_screencast_publishes_but_fires_no_connect_action`** fails about one run in 20 when run alone (1/20 on zenoh 1.10.1, 0/20 on 1.10.2 in one sample each, 2026-10-31), with `capture-armed screencast never fired the connect action`. The test toggles a screencast report off and on every 500 ms for 10 s, waiting for the capture-armed profile's `screen_off` to reach the mock TV; in a failing run none of the 20 toggles produced it.
- **`tisvlay-gui-app` `registered_callbacks_round_trip_gui_workflows`** failed once under full-suite load (2026-10-24) and passed in every other run. Not investigated.

## Risk

A red CI run that passes on retry hides a real regression in the same test. The nextest `ci` profile retries once, which masks both.

## Mitigation options

- For the screencast test: log what the session decided on each toggle (the armed set and the edge) to see whether the edge never fired or its action never reached the TV.
- Run each under `cargo nextest run --stress-count` (or a loop) to measure its rate before and after a change.

## Re-evaluate when

Either fails in CI, or a change to the stream gate or the GUI's callback wiring lands.
