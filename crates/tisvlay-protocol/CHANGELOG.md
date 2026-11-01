# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
