# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.1](https://github.com/TisVlay/TisVlay/compare/tisvlay-core-v0.5.0...tisvlay-core-v0.5.1) - 2026-11-01

### Other

- trim CLAUDE.md to what every session needs
- correct the smaller developer docs against the code

## [0.5.0](https://github.com/TisVlay/TisVlay/compare/tisvlay-core-v0.4.0...tisvlay-core-v0.5.0) - 2026-11-01

### Added

- *(cec)* count a power toggle only while the TV shows the PC
- make managed a kill switch for a TV's profile actions
- *(wakeup)* keep the PC asleep by blocking the adapter's port
- let the TV wake the PC from sleep
- let the TV remote's keys reach the desktop
- act on the PC when the TV's remote powers it off
- put the PC to sleep when the TV powers off by itself
- lock the session when the TV powers off by itself
- [**breaking**] rename remote sessions to streams
- keep retrying a local device that can't be opened
- wait for a local device node, and redial when it returns

### Other

- finish the stream rename in screencast_arms' doc

## [0.4.0](https://github.com/TisVlay/TisVlay/compare/tisvlay-core-v0.3.0...tisvlay-core-v0.4.0) - 2026-10-24

### Fixed

- *(gui,tray)* read a remote-off LG in activestandby as off ([#189](https://github.com/TisVlay/TisVlay/pull/189))
