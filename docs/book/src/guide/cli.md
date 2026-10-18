# Command Line

Everything in the TisVlay app is also available from a terminal with the `tisvlay` command, which is handy for scripts, SSH sessions, and automation tools. You never need it for ordinary use.

`tisvlay` is a pure client of the TisVlay service: every command below is a one-shot request to the service's control socket. Most commands that act on a TV take its **UUID** as the first argument; get it from `tisvlay list`. The global options (socket paths, `--json`) are listed under [`tisvlay`](#tisvlay) below.

## Which command does what the app does

| In the app | On the command line |
|------------|---------------------|
| **TVs** page: list and state | `tisvlay list` |
| **+** (add a TV) | `tisvlay discover`, then `tisvlay pair` |
| **Power on / off** | `tisvlay power <id> on\|off` |
| **Screen** switch | `tisvlay screen <id> on\|off` |
| **Backlight**, **Volume**, **Mute** | `tisvlay backlight`, `set-volume`, `mute` |
| **Current** input / **Primary** input | `tisvlay input`, `tisvlay primary-input` |
| **Managed** switch | `tisvlay managed <id> true\|false` |
| **PROFILES** dropdowns | `tisvlay assign`, `tisvlay user-profile assign` |
| **Profiles** page | `tisvlay profile …`, `tisvlay user-profile …` |
| **CONFIG** (edit a TV) | `tisvlay config`, `tisvlay config-set` |
| **Re-pair** / **Remove TV** | `tisvlay repair`, `tisvlay remove` |
| Sidebar **Reload** | `tisvlay reload` |
| **Enable TV control** banner | `tisvlay enable-access` |
| Sidebar status dots, **About** | `tisvlay status` |

## Seeing what's going on

- `tisvlay list` shows every TV and its current state (connection, power, input, …).
- `tisvlay status` checks that the service is up and reports its release.
- `tisvlay watch` streams every state change (history first, then live). It's the go-to for scripting and for watching automation happen.

## Scripting

Read commands take `--json` for machine-readable output, and `tisvlay watch` streams every change without polling, so a script can react to state instead of polling for it. The app and the tray use the same interface, so anything they show is scriptable.

## Tab completion

Tab completion ships for bash, zsh, and fish (installed by the packages; see [Installation](../getting-started/installation.md)). It's live: `<TAB>` completes not just commands and flags but TV UUIDs, profile UUIDs, and vendor ids, queried from the running service and annotated with their names, so you never type a UUID by hand.

## Command reference

The full command tree below is generated from the `tisvlay` source, so it always matches `--help`.

<!-- Generated from the `tisvlay` clap definitions by `just docs-gen`
     (crates/tisvlay-docgen); don't edit the partial by hand. -->
{{#include ../generated/cli.md}}
