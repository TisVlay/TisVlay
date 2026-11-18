# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.0](https://github.com/TisVlay/TisVlay/compare/tisvlay-protocol-v0.6.0...tisvlay-protocol-v0.7.0) - 2026-11-17

### Added

- *(protocol)* [**breaking**] decode stream_armed and discovered-TV fields strictly
- *(protocol)* type the profile-assignment and greeter payloads

### Fixed

- *(protocol)* take only zenoh's own timeout reply for a timeout

### Other

- *(protocol)* say that the caller's querier cache is never evicted
- *(protocol)* [**breaking**] give progress/power its payload path
- *(protocol)* [**breaking**] drop two unused key helpers, narrow the internal ones
- *(cli)* build the profile catalog keys through the protocol
- *(daemon,tray)* subscribe to the keys they read, by the protocol's names
- *(core,protocol,sdk)* correct the published crates' docs
- Rust 1.100.0

## [0.6.0](https://github.com/TisVlay/TisVlay/compare/tisvlay-protocol-v0.5.1...tisvlay-protocol-v0.6.0) - 2026-11-07

### Other

- No changes to this crate. It moves to 0.6.0 with the rest of the workspace, whose changes are in the unpublished crates (the settings GUI driven from a TV remote or gamepad): clients and services check each other's release on `major.minor`, so the published crates move with the binaries.

## [0.5.1](https://github.com/TisVlay/TisVlay/compare/tisvlay-protocol-v0.5.0...tisvlay-protocol-v0.5.1) - 2026-11-01

### Other

- No changes to this crate. It moves to 0.5.1 with the rest of the workspace, whose published crates share one version.

## [0.5.0](https://github.com/TisVlay/TisVlay/compare/tisvlay-protocol-v0.4.0...tisvlay-protocol-v0.5.0) - 2026-11-01

### Added

- *(protocol,sdk,daemon,session,tray,gui)* [**breaking**] say when a mirror's history has arrived
- make managed a kill switch for a TV's profile actions
- act on the PC when the TV's remote powers it off
- offer only the sleep modes logind grants the daemon
- lock the session when the TV powers off by itself
- [**breaking**] rename remote sessions to streams
- tell clients what a disconnected TV is waiting for

## [0.4.0](https://github.com/TisVlay/TisVlay/compare/tisvlay-protocol-v0.3.0...tisvlay-protocol-v0.4.0) - 2026-10-24

### Other

- No changes to this crate. It moves to 0.4.0 with the rest of the workspace: clients and services check each other's release on `major.minor`, so a 0.4 SDK expects a 0.4 protocol.
