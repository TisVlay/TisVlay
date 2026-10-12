# Mutation testing with cargo-mutants

Coverage says which lines ran; mutation testing says whether the tests would notice if those lines were wrong. `cargo mutants` rewrites one expression at a time (a `<` to `<=`, a function body to `Default::default()`), runs the tests, and reports each mutant that survived. The hand version of this, breaking a gate on purpose to see the test go red, is already the habit for new lints and property tests.

## Where it would run

- **Scoped to pure-logic crates:** core's types and helpers, the protocol's payloads, the world's reducers and systems, and the parsers (SSDP, SSAP decoding, EDID, CEC identity, MAC parsing). There, a mutant costs an incremental rebuild plus that crate's tests.
- **Not over the daemon, session or GUI stacks:** their tests boot zenoh and D-Bus, take seconds each, and would make every mutant slow and some flaky.
- **On demand, never a gate:** run locally on a crate, or on a PR label, with `--in-diff` so it only mutates the PR's changed lines; `--test-tool nextest` to match the runner.

## Trade-offs to budget for

- **Time:** one build and test run per mutant. Even scoped, a full pass over core or the world is minutes to hours; `--shard k/n` splits it.
- **Noise:** some survivors are equivalent mutants (the change doesn't alter behavior). Each needs a human look, so a report is a reading task, not a pass/fail.

## Trigger conditions

- A bug ships in code that has line coverage, so the tests ran it without checking it.
- CI is re-enabled and has slack for a labeled, non-blocking job.
