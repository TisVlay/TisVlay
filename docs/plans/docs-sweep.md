# Docs sweep

Bring the docs back in line with the code. Two read-only agent sweeps (2026-11-01) checked every doc against the code and found stale content in the user guide, developer docs that still describe the retired D-Bus stack, idea, concern and bug items that have shipped or no longer apply, broken links, and duplicated facts that have already drifted. The contributing guide now sends newcomers to these docs, so they need to be right.

Each item below names the file and the fix. Line numbers drift, so items quote the stale text's subject rather than a line. Verify each item against the code before changing it: the sweeps read the code, but didn't run it.

## Decisions

| Topic | Decision |
|---|---|
| Shipped or moot items | Delete them (git history is the archive), after moving any lasting rationale to its durable doc. Rewrite items whose gap still exists, in current terms. |
| Duplicates | One canonical home per fact; other copies become a clause and a link. |
| `CLAUDE.md` | Keep what every session must not get wrong; link everything else. |
| Process | This plan is the checklist. Commit it, the maintainer reviews it, implementation goes one commit per area, then one agent review at the end. Merge commit. |

Already done on this branch: the contributing plan is retired, the guide's links are root-relative (GitHub's Contributing tab resolves relative links from the repository root), and the issue chooser's duplicate security link is gone.

## 1. User guide (`docs/book/src/`)

Users read these, so they go first.

- [x] **`guide/configuration.md` and `operations/firewall-discovery.md`:** delete the `[discovery]` TOML blocks (`bind_port`, `poll_interval_ms`). The daemon has no such section and skips unknown fields, so an edit silently does nothing. Point to `--ssdp-port` and the NixOS `bindPort` option.
- [x] **`guide/automation.md`:** delete the "admin-set baseline" layer. User profiles resolve the assigned profile over the default, then the built-ins, as system profiles do.
- [x] **`reference/requirements.md`:** the daemon does use the system bus (logind). The session uses logind (`LockedHint`, remote sessions) and Mutter's IdleMonitor on GNOME, not the screensaver.
- [x] **`guide/streaming.md`:** `tisvlay watch` ignores `--user-socket`, and doesn't need it: a plain `tisvlay watch` shows the daemon's `stream/state` mirror (checked on a dev stack, 2026-11-01).
- [x] **Hand-copied defaults** in `guide/streaming.md` (the stream event values, Sunshine's port), `guide/automation.md`'s default table and `getting-started/quick-start.md`: name the event or flag and link to the generated defaults or CLI reference. Keep a table only where it explains meaning, not values.
- [x] **`operations/troubleshooting.md`:** "typically 1–5" seconds for a woken TV is below the repo's own measurement (5–12 s). Drop the number.
- [x] **`guide/gui-tray.md`:** use the tray's full labels ("Backlight Up (+10)", "Volume Up (+10)").

## 2. Retired D-Bus described as current

- [x] **`docs/discovery.md`:** rewrite the rpc sections in zenoh terms (`manager/rpc/discover`, `discover-start`, `discover-stop`, `enrich`), and state the real cadence: a 3 s scan and 2 s sleep with no hard cap, and no `poll_interval_ms`. `tisvlay list` reads caches. Drop the line count. The rationale sections stand.
- [x] **`packaging/nix/README.md`:** describe what `test.nix` actually tests, drop the `Manager1` and D-Bus claims, the `docs/todo.md` bullet (the firewall options ship), the hard-coded `version = "0.1.0"`, and the D-Bus reload. `config.toml` → `daemon.toml`. Point at `architecture.md`, not `design.md`.
- [x] **`docs/diagnostics.md`:** correct each field's source (`manager/vendors`, `row.online`, the hard-coded `runtime.bus`), the hand-rolled build env vars rather than `vergen-gix` (also in `diagnostic.rs` comments), and where the snapshots are taken (`about.rs`).
- [x] **`docs/cec/protocol.md`:** restate `Tv1.*`, `ConnectionChanged` and the D-Bus wording in terms of the identity and state keys. Drop the 20 s poll loop (poll-only backends refresh on reconnect), the stale What's Next pointer, and the `design.md#capabilities` link (point to `AutomateWhen::should_automate`).
- [x] **`docs/lg/firmware-quirks.md`, `docs/cec/testing.md`:** re-point regression coverage from `tisvlayd/tests/` and retired names (`tv_manager`, `await_power_state`, `state_forwarder`) to the current tests, and update the WoL boot timing to the benchmark.

## 3. ScreenSaver and other removed code

- [x] **`docs/testing.md`:** remove `FakeScreenSaver` and its sample; fix the Slint testing pin and crate, the `ci` slow-test timeout, the `cov-ratchet` and llvm-cov pipeline claims, the dbus-daemon-per-binary contradiction, and the suite lists (world has 11 suites; daemon adds `supervisor` and `wakeup`). Link the fuzzing and mutation ideas instead of restating them.
- [x] **ScreenSaver as consumed:** `.claude/CLAUDE.md`, `docs/architecture.md`, `docs/backends/implementing-a-vendor.md`. The real list is logind, Mutter IdleMonitor, ksni, notifications and GUI activation. Also the rustdoc in `tisvlay-test/src/dbus.rs`.
- [x] **`docs/backends/implementing-a-vendor.md`:** state the real state contract (refresh on reconnect plus a `get_state` after power and screen commands; no periodic poll, no `watch::Sender<PowerState>`), rewrite the verify section around `DispatchVerify`, add the missing capabilities and trait methods, and fix the `protocol.md` name (`docs/lg/` has `webos.md`).
- [x] **`docs/i18n.md`:** list only real recipes (`i18n-extract`, `i18n-update`), drop the tray's nonexistent `lang/` and `build.rs`, and fix the `cargo-about` fallback's crate. Move the tray's planned layout into `ideas/switchover-followups.md`.

## 4. Other developer docs

- [x] **`docs/build.md`:** three crates have a `build.rs`, not one or two; runtime's feature list; drop the `design.md` pointer for Linux-only; reconcile nightly being both "never adopted" and a review trigger.
- [x] **`docs/architecture.md`:** add `event/dispatch-failed` to the key table.
- [x] **`packaging/README.md`:** two binaries, not five; the Bazzite record's "system bus" connection (date the record); the Docker image is Ubuntu 26.04. Link to `cec/protocol.md` for the Pulse-Eight steps rather than restating them.
- [x] **`docs/licensing.md`** and `packaging/licensing/about.toml`: published crates are checked against `accepted`, not skipped; `sources` also allows the `linux-cec` fork; `installed_notices_path()` is in `about.rs`.
- [x] **`docs/watchers/network.md`:** `primary_descriptor()` (also in `core/src/traits.rs`'s module doc), the UNKNOWN-and-not-dormant fallback, and the `Reconnecting` component.
- [x] **`docs/streams/detection.md`:** `tisvlay service session --sunshine-port`; link the book for the Sunshine `global_prep_cmd` recipe and the stock defaults. Its claim that a plain `tisvlay watch` shows the `stream/state` mirror holds (checked live).
- [x] **`.claude/slint.md`:** the `Btn`, `ThemedCheckBox` and `ThemedTextInput` examples (gone; use `IconBtn` or `LineEditField`), and `open_notices_html` in `about.rs`.
- [x] **`docs/gui/`:** `HelpTrigger` is in `ui/globals/help.slint`; `accent.rs` is in `tisvlay-gui-app`.
- [x] **`docs/lg/testing.md`:** drop the fake server's line count.
- [x] **`docs/kde/tray_issue.md`:** retire it into `docs/bugs/upstream-kde-tray-submenu-collapse.md`, like the upstream linux-cec entries: the symptom (a nested submenu closes on its first expansion after the tray starts; still reproducible 2026-11-01), KDE bug 506439 and its MRs, the untested `about_to_show` workaround, and what to check on a newer Plasma. The old tray-side fixes stay in git history.

## 5. Ideas, concerns and bugs

Retire (delete, after moving any lasting rationale):

- [x] `bugs/last-action-cache-survives-reconnect.md`: the cache doesn't exist.
- [x] `ideas/seat-watch-fake-logind-coverage.md`: shipped in `seat/tests.rs`.
- [x] `ideas/dispatch-test-event-driven-waits.md` and `concerns/session-event-tests-real-time-sleeps.md`: the tests and sleeps they describe are gone.
- [x] `ideas/asv-wire-format-migration.md`: moot without served D-Bus; `ideas/typed-update-config.md` holds the live version.
- [x] `ideas/switchover-followups.md`'s coverage-baseline item: done.
- [x] `ideas/gui-test-coverage-fanout.md`: retire, or rewrite to the remaining gaps if any survive a check.

Rewrite in current terms:

- [x] `bugs/pair-tv-no-timeout.md`: the gap survives in `tisvlay-world`'s `pairing_task`, which waits with no deadline for non-LG vendors.
- [x] `ideas/ci-nightly-build-knobs.md`: `-Zthreads` was measured and rejected.
- [x] `ideas/greeter-integration-generalization.md` and `concerns/README.md`'s greeter entry: the Mutter fallback and GDM wiring shipped.
- [x] `concerns/zenoh-idiom-audit.md`: mark B1, B3 and B4 done, or retire it if nothing remains.
- [x] `ideas/probe-library-extraction.md`, `ideas/connected-power-on.md`: current names and paths.
- [x] Retired names in live ideas: `dispatch-outcome-return-type`, `event-driven-power-management`, `pair-time-mac-capture`, `notification-subsystem-followups`, `polkit-multi-user`, `nixos-user-service-auto-start`, `support-bundle-cli`, `ci-artifact-smoke-test`, `open-logs-affordance`, `wol-raw-socket`, `ssdp-notify-passive-listener`, `tv-side-power-prevention`, `cec-followups`. Retire any that turn out moot.

## 6. Links

- [x] `git mv` `concerns/remote-session-detector-validation.md` → `stream-detector-validation.md` and `ideas/remote-session-followups.md` → `stream-followups.md`, matching their index entries; fix inbound links.
- [x] `docs/firewall.md`'s dead anchor; `docs/kde/gui-application-activation.md` → `tray/src/lib.rs`; `crates/tisvlayd/…` links in ideas; `ideas/gui-test-coverage-fanout.md`'s dead anchor (if it survives step 5).

## 7. Duplicates and `CLAUDE.md`

- [x] **SSDP and the firewall, split by job:** recipes live only in the book's Firewall & Discovery page; `docs/firewall.md` keeps only the network evidence (how SSDP replies arrive, why Linux alone drops them, conntrack, the captures, why other projects don't hit it), so the code comments pointing there stay right; `docs/discovery.md` keeps our design, and the research it shares with `firewall.md` stays in one of them. The mDNS alternative moves to an idea doc.
- [x] **Zenoh laws:** canonical in `docs/architecture.md`. `CLAUDE.md`'s copy has drifted (it misses the "dialed hub does observe" correction and the querier-matching gate). Keep one-liners and a link, or only the laws a session most often trips on.
- [x] **Test layers:** canonical in `docs/testing.md`; `CLAUDE.md`, `architecture.md` and the vendor guide link to it.
- [x] **WoL boot latency:** canonical in the re-measured benchmark (area 8); `CLAUDE.md`'s Known Quirks and `firmware-quirks.md` link to it.
- [x] **Socket access:** canonical in `packaging/README.md`.
- [x] **`CLAUDE.md` trim:** cut what the guide or a canonical doc now holds (Running the Stack's basic commands, the recipe block, the testing summary, the CI paragraph, duplicated reference entries, the i18n workflow beyond its traps). Fix the project tree (`tisvlay-icons`, `docs/backends/`, `packaging/` subdirectories), the "root-D-Bus-client-compatible" phrase, and What's Next. Re-point `docs/testing.md`'s reference to the Tasks section and `pairing_schema.rs`'s reference to CLAUDE.md's i18n section.

## 8. Re-measure wake timing

`docs/lg/benchmarks/wake-reconnect.md` compares pre-0.3.0 reconnect designs (NetworkManager watcher variants) measured with `busctl`, and `scripts/bench_network.py` matches log lines that no longer exist. Only its TV hardware timings still hold. Re-measure on the current stack and rewrite both.

- [ ] Rewrite `scripts/bench_network.py` for the current log lines and keys (to be designed with the maintainer).
- [ ] Run it as an unattended script logging to a file, since the TV goes dark.
- [ ] Rewrite the doc with current numbers, dated, and its hardware observations re-checked. Link it from the book's Troubleshooting page, `CLAUDE.md` and `firmware-quirks.md` instead of quoting numbers.

## Deviations

- **Area 1:** the "What happens by default" table in Automation and the list in Quick Start stay. They describe behavior in the user's terms, which is what the pages are for, rather than copying config values. Streaming's defaults became plain-language behavior, and Sunshine's port links to the generated CLI reference.

- **Area 2:** `firmware-quirks.md` keeps its Fix notes in the pre-0.3.0 code names, with a note saying where that logic lives now, rather than rewriting each fix. Two old tests have no successor: CEC identity following a dongle swap, and an LG `screen_off` accepting `ActiveStandby` when the wake refuses. The docs say so; they aren't filed as concerns yet.

- **Area 3:** a finding beyond the plan: a poll-only network backend can't verify its screen actions, since the daemon refreshes such a backend's cached state only on connect. The vendor guide now says so; it isn't filed as an idea yet. The tray's planned i18n layout and its two design choices moved to `ideas/switchover-followups.md`.

- **Area 4:** `docs/streams/detection.md` keeps its "stock story" of the stream defaults, since it explains why they are what they are; only the Sunshine hook recipe became a link. `packaging/README.md` keeps its one-clause Pulse-Eight mention, consistent with `cec/protocol.md`. `docs/kde/gui-application-activation.md`'s dead link, an area 6 item, was fixed here.

- **Area 5:** judgment calls. `gui-test-coverage-fanout.md` was retired, not rewritten: its premise (the `TestGui` harness, 52% coverage, one canonical test) is gone, and a fresh gap list would start from a coverage report. `probe-library-extraction.md` was retired: the GUI shipped without needing EDID detail, and its rationale now sits in the SDK's `desktop.rs` doc comment. `zenoh-idiom-audit.md` stays, marked done, since its "don't fix these" list still applies. `notification-subsystem-followups.md` lost its two D-Bus-era items; its open question resolved to a finding (a `tray.toml` missing a switch doesn't parse), which is now the whole idea. `ci-nightly-build-knobs.md` records the 2026-10-04 measurement. The switchover idea's name-sweep item is done by this area and was removed.

- **Area 7:** the test-layer summaries in `architecture.md` and the vendor guide stay: each is already a sentence and a link to `testing.md`. The WoL latency now links to the benchmark doc, which area 8 rewrites; until then it holds the old numbers. `CLAUDE.md` went from 24.7 KB to 19.9 KB (22.9 KB with the imported conventions, against 27.6 KB before the contributing guide).

## Verification

- `prek run -a` (file checks and typos) and `just docs-links` after each commit; `just docs-gen-check` after any book change.
- A final relative-link check over `docs/`, `.github/` and `.claude/`.
- One agent review at the end: accuracy of the rewritten docs against the code.
- After merge: the Contributing tab's root-relative links resolve.
