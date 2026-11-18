# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.0](https://github.com/TisVlay/TisVlay/compare/tisvlay-v0.6.0...tisvlay-v0.7.0) - 2026-11-17

### Added

- *(core)* publish the in-place wake grace once
- *(protocol)* type the profile-assignment and greeter payloads
- *(world)* refuse a TV profile that names no profile on every path

### Changed

- `DaemonClient::screen_off` (and `apply_action` with a screen-off) is refused with `ErrorCode::NotConnected` for a TV that was on when last seen and can no longer be reached, where any disconnected TV used to ack. The daemon now keeps dialing such a TV until it answers and holds nothing for it meanwhile, so a request made during a network outage is not applied later: send it again once `TvInfo::connected` is true. A TV that turned off still acks a screen-off as already dark ([#255](https://github.com/TisVlay/TisVlay/pull/255)).
- `DaemonClient::power_off` to such a TV is refused the same way even while a wake for it is pending; it used to ack after cancelling the wake, which it still cancels.
- `state/link` reads `reconnecting` again after a drop the TV did not announce, and for a TV that has not answered since the daemon started, about once a minute for as long as the outage lasts; such a TV used to get three dials.

### Fixed

- *(sdk)* leave no ghost row when a removed TV's keys arrive out of order
- *(sdk)* release a client's session when its last copy drops
- *(sdk)* let a zero fallback poll interval turn the poll off
- *(sdk)* undeclare a progress stream's subscriber when it's dropped
- *(world)* hold nothing for a TV being redialed through an outage

### Other

- *(sdk)* project list_tvs from the mirror row in one pass
- *(sdk)* [**breaking**] make the mirror reads plain functions
- *(sdk)* [**breaking**] mark StateEvent non-exhaustive
- *(sdk)* [**breaking**] one pairing drain, not a pinned twin
- *(protocol)* [**breaking**] give progress/power its payload path
- *(core,protocol,sdk)* correct the published crates' docs
- Rust 1.100.0

## [0.6.0](https://github.com/TisVlay/TisVlay/compare/tisvlay-v0.5.1...tisvlay-v0.6.0) - 2026-11-07

### Other

- No changes to this crate. It moves to 0.6.0 with the rest of the workspace, whose changes are in the unpublished crates (the settings GUI driven from a TV remote or gamepad): clients and services check each other's release on `major.minor`, so the published crates move with the binaries.

## [0.5.1](https://github.com/TisVlay/TisVlay/compare/tisvlay-v0.5.0...tisvlay-v0.5.1) - 2026-11-01

### Fixed

- `DaemonClient::power_cycle_progress_stream` is live from the call on: it no longer replays an earlier wake's last phase, which read as the new wake's outcome ([#230](https://github.com/TisVlay/TisVlay/pull/230))

### Other

- retire shipped ideas and restate the live ones

## [0.5.0](https://github.com/TisVlay/TisVlay/compare/tisvlay-v0.4.0...tisvlay-v0.5.0) - 2026-11-01

### Added

- *(sdk,tray)* [**breaking**] render the tray from the SDK clients' mirrors
- *(sdk,session)* follow the daemon through a DaemonClient in the session
- *(protocol,sdk,daemon,session,tray,gui)* [**breaking**] say when a mirror's history has arrived
- *(sdk)* [**breaking**] follow the session and tray through the lifetime mirror
- offer only the sleep modes logind grants the daemon
- [**breaking**] rename remote sessions to streams
- tell clients what a disconnected TV is waiting for

### Fixed

- *(daemon)* arm streaming TVs again when the session's link returns
- *(sdk)* follow the daemon through one lifetime-scoped mirror
- *(sdk,tray)* subscribe before announcing the daemon, own every task
- re-mirror the daemon when its token returns after a link drop

### Other

- *(daemon,session,tray,greeter)* send miss heartbeats only after a change
- *(sdk)* run every mirror through one link-breaking harness

## [0.4.0](https://github.com/TisVlay/TisVlay/compare/tisvlay-v0.3.0...tisvlay-v0.4.0) - 2026-10-24

### Added

- *(sdk)* [**breaking**] mirror service releases and render the GUI from one loop ([#183](https://github.com/TisVlay/TisVlay/pull/183))

### Fixed

- harden desktop workflows with hardware-free regression tests ([#182](https://github.com/TisVlay/TisVlay/pull/182))

### Other

- clarify project and SDK badges
- move the bugs, ideas and concerns indexes into their folders ([#179](https://github.com/TisVlay/TisVlay/pull/179))
