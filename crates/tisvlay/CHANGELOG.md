# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
