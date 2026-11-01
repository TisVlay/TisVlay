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
| **Backlight**, **Volume**, **Mute** | `tisvlay backlight`, `tisvlay volume`, `tisvlay mute` |
| **Current** input / **Primary** input | `tisvlay input`, `tisvlay primary-input` |
| **Managed** switch | `tisvlay managed <id> on\|off` |
| **PROFILES** dropdowns | `tisvlay system-profile`, `tisvlay user-profile` |
| **Profiles** page | `tisvlay profile system …`, `tisvlay profile user …` |
| **CONFIG** (edit a TV) | `tisvlay config` |
| **Re-pair** / **Remove TV** | `tisvlay repair`, `tisvlay remove` |
| Sidebar **Reload** | `tisvlay reload` |
| **Enable TV control** banner | `tisvlay enable-access` |
| Sidebar status dots, **About** | `tisvlay status` |

## Seeing what's going on

- `tisvlay list` shows every TV and its current state (connection, power, input, …).
- `tisvlay status` checks that the service is up and reports its release.
- `tisvlay watch` streams every state change (history first, then live). It's the go-to for scripting and for watching automation happen.

## Reading and changing a TV

Every per-TV setting and state works the same way: give a value to change it, leave the value out to read it.

```sh
tisvlay volume <id> 30        # set the volume
tisvlay volume <id>           # print it: 30
tisvlay mute <id> --toggle    # flip mute, printing what was sent: on
```

- A read prints what you'd type to set it (`on`/`off`, a number, an input or profile id), so its output can be fed straight back in. With `--json` it prints the bare JSON value.
- On/off settings accept `on`/`off` (or `true`/`false`, `yes`/`no`, `1`/`0`) and take `--toggle`.
- Settings that can be unset (the primary input, the assigned profiles) take `--clear`. A TV with no assigned profile reads as the default profile's id; an unset primary input prints nothing.
- Commands that change something print nothing when they succeed. A failure exits non-zero with the reason on stderr.
- A reader that stops early, as in `tisvlay watch | head`, ends the command quietly with status 0.
- `power` reads `on` only when the TV is connected and awake, so a TV switched off with its remote reads `off`, and `power --toggle` wakes it. A TV whose screen alone is off reads `on`, and `power on` leaves it so; `screen on` lights it.
- Values the TV reports, such as volume or the current input, come from its last report. If the TV is disconnected the command still prints the value, with a warning on stderr that it may be stale; if the TV hasn't reported it since the service started, the command fails.
- Two `--toggle`s in quick succession can both see the old value, so a hotkey can miss a very fast double press.
- In a script, quote a read you pass on to another command: an empty result would otherwise turn the second command into a read.

## Scripting

Read commands take `--json` for machine-readable output, and `tisvlay watch` streams every change without polling, so a script can react to state instead of polling for it. The app and the tray use the same interface, so anything they show is scriptable.

## Tab completion

Tab completion ships for bash, zsh, and fish (installed by the packages; see [Installation](../getting-started/installation.md)). It's live: `<TAB>` completes not just commands and flags but TV UUIDs, profile UUIDs, and vendor ids, queried from the running service and annotated with their names, so you never type a UUID by hand.

## Command reference

The full command tree below is generated from the `tisvlay` source, so it always matches `--help`.

<!-- Generated from the `tisvlay` clap definitions by `just docs-gen`
     (crates/tisvlay-docgen); don't edit the partial by hand. -->
{{#include ../generated/cli.md}}
