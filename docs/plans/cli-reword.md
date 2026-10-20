# CLI rework: one grammar for TV attributes, profiles split by scope

The `tisvlay` CLI grew a verb at a time, and it shows: one attribute takes a `set-` prefix (`set-volume`) while the rest are bare nouns, toggles take `on|off` in some places and `true|false` in others, reading a value means parsing `list --json`, and profile assignment is split between a top-level `assign` (system) and `user-profile assign` (user). This plan settles one grammar and applies it to the whole tree in a single breaking release.

No compatibility aliases or did-you-mean hints for old names: they're replaced outright, per the project's no-shims rule. The CLI is not a wire surface (no key, payload or rpc changes), but scripts call it, so the change ships as a breaking `feat(cli)!` and the PR body carries the old → new table as its release note (`tisvlay-cli` is unpublished, so release-plz's changelogs skip it, and the squash-merged PR title is what lands on `main`).

## The grammar

**Getter/setter.** Every per-TV attribute is one verb, `<attribute> <tv> [<value>]`: with a value it sets, without one it prints the current value. The verbs: `power`, `screen`, `mute`, `managed`, `volume`, `backlight`, `input`, `primary-input`, `name`, `config`, `system-profile`, `user-profile`.

**Flags that replace a value** (each conflicts with a positional value):

- `--toggle` on every on/off attribute (`power`, `screen`, `mute`, `managed`): reads the current value, sends the opposite, and prints the value it sent (requested, not confirmed).
- `--clear` on the attributes that can be unset: `primary-input`, `system-profile`, `user-profile`. Leaving the value out used to clear these; it now reads.
- `--effective` on `system-profile` and `user-profile`: prints the TV's resolved profile (the assigned profile layered over the stock defaults). It replaces the catalogs' `effective` subcommands.

**Values.** One `OnOff` `ValueEnum` with aliases: help and completion show `on|off`; `true/false`, `yes/no` and `1/0` are accepted. A typed enum rather than clap's `BoolishValueParser`, so help lists exactly two values and getters print through the same type. Empty strings are rejected at parse time for `input`, `primary-input` and `name` (today `primary-input <tv> ""` stores `Some("")`).

**Getter output.** A getter prints what its setter accepts, so output can be fed straight back in. `--json` prints the bare value, not the state object it comes from: `list --json` and `watch` already give the whole objects.

| Verb | Prints | `--json` | Source |
|------|--------|----------|--------|
| `power` | `on` when connected and awake, else `off` | the cached substate, e.g. `"activestandby"` | `state/link` + `state/power` |
| `screen` | `on` when connected and the panel shows a picture, else `off` | `true` / `false` | `state/link` + `state/power` |
| `mute` | `on` / `off` | `true` / `false` | `state/volume.muted` |
| `volume`, `backlight` | the level, e.g. `30` | `30` | `state/volume`, `state/backlight` |
| `input` | the current input ID | `"HDMI_2"` | `state/input.current` |
| `managed` | `on` / `off` | `true` / `false` | `GetConfig` rpc |
| `name` | the name | `"Den"` | `GetConfig` rpc |
| `primary-input` | the input ID, or nothing when unset | the ID, or `null` | `GetConfig` rpc |
| `system-profile` | the assigned profile UUID; the default profile's UUID when unassigned | the UUID | `GetConfig` rpc |
| `user-profile` | the same, for the user profile | the UUID | `user/tv/<tv>/profile` (user socket) |
| `--effective` | `key = value` lines (as today) | the object (as today) | `effective` rpcs |
| `config` | `key = value` lines (as today) | the config view (as today) | `GetConfig` rpc |

"Awake" is `PowerState::is_awake`, the complement of the daemon's wake check (`needs_wake_preflight`): active, screensaver and screen-off count as on; everything else, including LG's `activestandby` after a remote power-off, counts as off. The GUI's Power switch and the tray once counted `activestandby` as on; [docs/bugs/power-switch-active-standby.md](../bugs/power-switch-active-standby.md) moved them onto the same rule. A CEC TV stays connected in standby, so the rule holds there too.

An unassigned profile prints the reserved default's UUID (`DEFAULT_SYSTEM_PROFILE_UUID` / `DEFAULT_USER_PROFILE_UUID`), since that's the profile the TV follows, and it round-trips into the setter. `primary-input` has no default, so unset prints nothing; an unquoted `$(…)` of an empty result turns the setter into a getter, which the guide notes.

Two exceptions to round-tripping: `config` takes a merge patch but prints the full config view (which also carries `id` and `vendor`), and `--effective` prints a resolved body with no setter. Both are rpc reads, so the missing/stale rules below don't apply to them.

**Missing, unsupported and stale values.** Device state (`state/*`) comes from the publisher caches, the way `list` reads it; daemon facts come from `GetConfig`, which also answers "does this TV exist" with a typed not-found in one round trip.

- Unknown TV (`GetConfig` not-found): error, non-zero exit.
- The TV's vendor doesn't advertise the capability (`meta/identity.capabilities`, the same flags that hide the GUI's rows): error "TV doesn't support <attribute>", non-zero exit. This also covers `mute`, whose `muted` is `false` when the backend can't report it.
- The TV has never reported the value: error naming the value, non-zero exit. A daemon restart starts every TV with no `state/*` keys until it first connects, so a TV asleep since boot lands here for `volume`, `mute`, `backlight` and `input`.
- `state/link` says disconnected: `volume`, `mute`, `backlight` and `input` print their last value and warn on stderr that it may be stale (keys persist across a disconnect; only removing the TV deletes them). `power` and `screen` need no warning: a disconnected TV reads `off` by the rule above, with or without a cached `state/power`. `power --json` prints that raw cached substate, so it does warn.

The cache read drains one subscription on the TV's keys until every needed key has arrived or the quiet window expires: one combined predicate, per the zenoh ordering law. A key that's legitimately absent can't end the drain early, so `user-profile <tv>` on an unassigned TV (the session deletes the key on unassign) always waits out the quiet window.

**`--toggle` and the link.**

- `power --toggle` on a disconnected TV powers it on, through the daemon's wake path. A TV without wake support (e.g. an LG with no MAC) fails with the daemon's `wake_not_supported`. For about 5 s after a remote power-off, LG firmware ignores network wake requests (docs/lg/firmware-quirks.md); a toggle in that window sends the right command but the TV won't wake.
- `screen --toggle` on a disconnected TV turns the screen on: the daemon sends a disconnected `screen_on` down the wake path.
- `mute --toggle` on a disconnected TV fails: there's no link to act on.
- `managed --toggle` is a daemon setting and works regardless of the link.
- Two toggles in quick succession can both read the old value (the set's acknowledgement and the state update travel separately), so a hotkey bound to `--toggle` can miss a fast double press. The guide notes it.

**Writer output.** Silent on success, like `systemctl`, `git config`, `pactl` and `nmcli con mod`, with `--json` or without:

- Setters, `--clear`, `remove`, `reconnect`: print nothing.
- `--toggle`: prints the value it sent, `on`/`off` (`--json`: `true`/`false` for all four verbs, including `power`, whose sent value is a direction, not a substate).
- `profile system|user create|duplicate`: print the new UUID alone, so `id=$(…)` works.
- `pair` and `repair`: progress lines on stderr, the TV's UUID on stdout at completion.
- Errors stay on stderr with a non-zero exit, as today.

## The command tree

Old → new (this table goes into the PR body):

| Today | After |
|-------|-------|
| `set-volume <tv> <level>` | `volume <tv> [<level>]` |
| `backlight <tv> <level>` | `backlight <tv> [<level>]` |
| `power <tv> on\|off` | `power <tv> [on\|off \| --toggle]` |
| `screen <tv> on\|off` | `screen <tv> [on\|off \| --toggle]` |
| `mute <tv> true\|false` | `mute <tv> [on\|off \| --toggle]` |
| `managed <tv> true\|false` | `managed <tv> [on\|off \| --toggle]` |
| `input <tv> <input>` | `input <tv> [<input>]` |
| `primary-input <tv> [<input>]` (no value cleared) | `primary-input <tv> [<input> \| --clear]` |
| `rename <tv> <name>` | `name <tv> [<name>]` |
| `config <tv>`, `config-set <tv> <patch>` | `config <tv> [<patch>]` |
| `assign <tv> [<profile>]` | `system-profile <tv> [<profile> \| --clear \| --effective]` |
| `user-profile assign <tv> [<profile>]` | `user-profile <tv> [<profile> \| --clear \| --effective]` |
| `profile <verb>` | `profile system <verb>` |
| `user-profile <verb>` | `profile user <verb>` |
| `profile effective <tv>`, `user-profile effective <tv>` | `system-profile <tv> --effective`, `user-profile <tv> --effective` |
| `vendors`, `schema <vendor>` | `vendor [<vendor>]` |
| `discover [--timeout-ms N]` | `discover [--timeout <duration> \| --watch]` |
| `discover-session` | `discover --watch` |
| `add <name> --address … [--unmanaged]` | removed |
| `pair … [--address A]` | `pair … --params JSON [--unmanaged]` (`--address` removed) |
| writers print `ok` / `created: <id>` / `removed` | silent; `create`/`duplicate`/`pair` print the bare UUID |

Unchanged: `status`, `reload`, `enable-access`, `list`, `watch`, `repair` (it reads well next to `remove`), `reconnect`, `remove`, `remote-session {start,stop,status}`.

The shape, grouped by purpose:

```
tisvlay [--socket P] [--user-socket P] [--site S] [--json] <command>

  status | reload | enable-access | list | watch

  discover [--vendor V] [--timeout 3s | --watch]
  pair <name> [--vendor V] --params JSON [--unmanaged]
  repair <tv> | reconnect <tv> | remove <tv>
  vendor [<vendor>]

  power | screen | mute | managed  <tv> [on|off | --toggle]
  volume | backlight               <tv> [<0-100>]
  input                            <tv> [<input>]
  primary-input                    <tv> [<input> | --clear]
  name                             <tv> [<name>]
  config                           <tv> [<patch>]
  system-profile | user-profile    <tv> [<profile> | --clear | --effective]

  profile system | user  {list, create, update, duplicate, delete, defaults}
  remote-session         {start, stop, status}
```

clap prints one subcommand per line under a single heading, so `--help` can't reproduce these groups: `display_order` clusters the verbs in this order, `after_long_help` carries a short grammar summary (getter/setter, `--toggle`, `--clear`), and TV positionals use `value_name = "TV"`.

TV attributes stay flat at the top level: they're the most-typed verbs, and a `tv` group would lengthen every command. `profile` gains a scope level because system and user profiles are different schemas (`SystemProfileConfig` holds the power-event policy; `UserProfileConfig` holds lock, idle and remote-session behavior), not one schema at two scopes, so a `--user` flag that silently changes what `--body` means would mislead. The split mirrors the GUI: the Profiles page lists **SYSTEM PROFILES** and **USER PROFILES** as separate sections, and assignment lives on the TV page's **PROFILES** section as one dropdown per scope, which `system-profile <tv>` / `user-profile <tv>` mirror. `primary-input` is its own verb, not an `input --primary` flag, because it's a daemon setting (never stale, can be unset) where `input` is device state.

`add` goes with no replacement verb: enrolling without pairing stays an rpc (`manager/rpc/add-tv`, which the daemon tests use) with no CLI surface. `pair --address` goes with it: it only ever set the mock's `address`, and the mock's pairing mode defaults to `manual`, which waits for a test handle a production daemon doesn't have, so `pair --vendor mock --address …` hangs outside tests. The mock health check passes the mode explicitly: `pair "mock-test" --vendor mock --params '{"address":"mock://test","pairing":"accept"}'`.

## Implementation

All in `tisvlay-cli`; no other crate changes. `--timeout` rejects a window over the daemon's scan cap at parse time instead of letting the daemon clamp it silently, reading the cap through the public `manager::discover_scan` (which clamps every window, so asking it for the longest reads the cap) rather than exporting the `pub(super)` constant. `pair --unmanaged` rides the existing `managed` field of the pair request (`rpc::AddTv`), which the CLI hard-codes to `true` today and which reaches enrollment intact.

**First, a pure-move split of `main.rs`** (1349 lines, one `match`), precedent `refactor(cli)` 6f6f5f13: `tv.rs` (attribute verbs), `profiles.rs`, `pairing.rs` (discover, pair, vendor, `link_mac_line`), `render.rs` (`format_fields`, `print_reply`), `reload.rs`, `status.rs`; `main.rs` keeps dispatch, the release checks and the session opens. No behavior change, so the rework's diff shows only the rework.

**Then the rework:**

- **`lib.rs` (the clap tree).** Rework `Command` per the table. `Profile` takes a `ProfileScope` subcommand (`System(SystemProfileCmd)`, `User(UserProfileCmd)`); the two catalog enums lose `Effective` (and the user one `Assign`). Add `OnOff`. Attribute verbs take `Option<value>` plus their flags, with clap `conflicts_with` doing the exclusion, so no hand-rolled argument validation. Completion helper docs that name old verbs (`assign`, `schema`, `user-profile`) are updated, and the module doc's "the lib exists for docgen" note widens to the shared hydrate helper below.
- **One `hydrate` helper** (`pub` in `lib.rs`, since the bin reaches the lib only through `pub` items; the crate is unpublished): `hydrate(session, keyexpr, quiet, done)` drains an AdvancedSubscriber's history until `done` holds or the stream goes quiet. It replaces the four existing copies (`list` 400 ms, `list_catalog` 600 ms, and the two completion collectors at 150 ms), each keeping its own quiet window.
- **A typed `TvSnapshot`.** `read_tv` uses `hydrate` on the TV's keys and decodes `payload::{Power, Volume, Link, Input, Backlight, Identity}` by their `PATH` constants, replacing `Value` indexing like `v["connected"] == true`. One `print_fact` owns the missing/unsupported/stale and `--json` rules.
- **Sessions and release checks.** The user-socket open (copied into the `UserProfile` and `RemoteSession` arms today) becomes one `open_user`. `profile user` and `remote-session` skip the daemon release check and check the session service's (a nested `Command::Profile(ProfileScope::User(_))` pattern in the skip list). `user-profile <tv>` uses both: `GetConfig` on the daemon for the unknown-TV check, the assignment key on the user socket, and both releases checked.
- **`vendor`** merges the `vendors` and `schema` arms: no argument prints the catalog, an argument prints that vendor's row and pairing fields (`--json`: the catalog entry with a `schema` array). An unknown vendor is an error.
- **`discover`.** `--timeout` is an `Option<Duration>` parsed as a `jiff::SignedDuration` (friendly `3s`/`500ms` or ISO 8601; `jiff` with only its `std` feature, so no time-zone database) and checked against the cap; unset, the daemon applies its default. `--watch` is today's `discover_session`, with one fix: it installs its SIGINT listener before printing "ctrl-c stops", since `tokio::signal::ctrl_c()` installs the handler only when first polled, and a ctrl-c in that gap killed the process without stopping the scan session. `watch_pairing` gets the same listener.
- **Pairing.** `add`, `--address` and `address_config` go; `pairing_config` reduces to parsing `--params`. The link-MAC hint (`link_mac_line`) prints `tisvlay config <id> '{"mac":…}'`.

The client SDK (`tisvlay` crate) isn't used for getters: its `DaemonClient::connect` dials in the background and never fails fast (losing the CLI's "unreachable" and `enable-access` errors), and its mirror hydrates every TV and both catalogs with no "done" signal. The CLI stays on `Caller` and the protocol payload types, the layer the SDK is built on.

## Tests

- **Integration (`tests/it/workflows.rs`)**, against the mock stack booted there.
  - Update every existing invocation. Tests that used `add` to get a TV without pairing call `manager::AddTv` in-process through a `Caller`.
  - An `eventually(args, expected, deadline)` helper that polls a getter, used after every set and between toggles: the setter's ack and the `state/*` publish are separate channels, so a single read can see the old value.
  - A set-then-get round trip per attribute; `--toggle` flipping and printing the sent value; `--clear` on `primary-input` and both profile scopes; an unassigned profile printing the default UUID; `--json` bare values (and the substate for `power`); `--effective` on both scopes; `vendor` with and without an ID; `pair --unmanaged` landing `managed = off`; writers printing nothing, and `create` printing a bare UUID.
  - Error paths: an unknown TV, a never-reported value, and an unsupported capability each exit non-zero.
  - Disconnected TV: `power <tv> off` on the default (network-style) mock closes its connection; poll `list --json` until `state/link.connected == false`, then assert `power` reads `off`, `volume` prints with the stale warning on stderr, and `mute --toggle` fails.
  - `discover --watch`: spawn it, read the banner, send SIGINT, assert it stops cleanly. (It has no coverage today.)
- **Parser (`lib.rs` tests, `try_parse_from`, no daemon)**, next to `clap_definition_is_consistent`: `OnOff` accepts every alias, rejects other words, and its `Display` output parses back to the same value; the exclusions reject `mute <tv> on --toggle`, `primary-input <tv> X --clear`, `system-profile <tv> <id> --effective`, `discover --timeout 1s --watch` and a `--timeout` over the cap; empty `input`/`primary-input`/`name` values are rejected.
- The `link_mac_line` unit test's expected string changes with the hint.

## Docs and references

The generated CLI reference (`docs/book/src/generated/cli.md`) regenerates with `just docs-gen` in the same commit as the clap change, since `docs-gen-check` gates on drift. Hand-written references are swept with `grep -rnE "tisvlay [a-z-]+" docs packaging .claude crates/tisvlay-cli` and every hit reviewed, not an allowlist of renamed verbs: the `profile <verb>` → `profile system <verb>` move and the `true|false` → `on|off` spellings don't show up in a rename list. Known hits:

- Book: `getting-started/pairing.md` (`managed … false`, `repair`, the `config-set` MAC hint), `guide/automation.md` (the `profile …`, `assign`, `user-profile assign` block), `guide/cli.md` (the GUI ↔ CLI table, plus a short grammar section: getters, `--toggle`, `--clear`, exit codes, the stale warning, the double-toggle and empty-`$(…)` caveats; rules only, not a second verb list, since the generated reference already lists them), `guide/configuration.md`, `operations/firewall-discovery.md` (`discover-session`), `operations/troubleshooting.md` (the mock health check, `profile …`, `set-volume`).
- `.claude/CLAUDE.md` Quick Reference (`vendors`, `config-set`, `profile list`, `user-profile list`).
- `packaging/nix/test.nix` (the VM test runs `tisvlay … vendors`).
- `packaging/scripts/postinstall.sh`: its pairing hint passes `--json '{…}'` where `--params` is meant, an existing bug, fixed in the same sweep since it's the first CLI hint a new user sees.
- `docs/concerns/cli-pair-vendor-fields.md`, updated in place: its Context cites `build_pairing_config_json` and says the CLI can't carry vendor fields, both stale since `--params` landed. What remains is that the daemon merges no schema defaults, so the mock's `pairing` must be passed explicitly.
- Ideas: `completion-followups.md` (`assign`, `schema`; add input-ID completion, below), `sdk-cli-only-rpcs.md` and its `ideas/README.md` index line (`add-tv` keeps its rpc but loses its CLI verb; `user-profile effective` becomes `user-profile <tv> --effective`), `cli-update-config-shorthand.md` (its `tvs <name|uuid> set` shape predates this grammar; re-point it at `config <tv>` and at the TV-name idea), `typed-update-config.md` (`config-set`).
- `docs/plans/mdbook-docs-site.md` (`tisvlay schema`).
- Drive-by: `docs/architecture.md`'s topology table says the CLI connects as a connect-only peer; it opens a client-mode session (`bootstrap::client_config`).
- Left alone: `docs/design.md` (the retired ≤ v0.2.0 architecture, kept as history).

## Out of scope

- **TV names as selectors** (`tisvlay volume "Den" 30`): filed as [docs/ideas/cli-tv-name-selectors.md](../ideas/cli-tv-name-selectors.md).
- **`automate_when` as its own verb**: stays reachable through `config <tv> <patch>`.
- **Live input-ID completion for `input <tv> <TAB>`**: completers run before clap parses argv, so they can't see which TV was typed. Added to `completion-followups.md`.
- **The GUI and tray Power-switch rule**: filed as [docs/bugs/power-switch-active-standby.md](../bugs/power-switch-active-standby.md); fixing it touches the GUI and tray, not the CLI.

## Commits

1. ~~This plan and the TV-name idea, as a baseline.~~ Done (3afbb6f6); the review revision lands as its own commit on top.
2. `refactor(cli): split main.rs into modules`: pure move.
3. `feat(cli)!: …`: the clap tree, dispatch, `hydrate`/`TvSnapshot`, tests, the regenerated reference and the hand-written docs sweep, together: the drift gate ties the generated reference to the clap change, and a docs-only follow-up would leave `main` documenting verbs that no longer exist.
4. Retire this plan. User-facing rules are already in `guide/cli.md` (commit 3). Design reasons go next to the code they explain: doc comments on `ProfileScope` (why two scopes) and `read_tv`/`print_fact` (the awake rule, the stale rules, the toggle-on-disconnect choices), plus a short CLI paragraph in `docs/architecture.md`. A one-line convention goes in `.claude/CLAUDE.md` § Code conventions ("a new per-TV attribute is a getter/setter verb, `<attribute> <tv> [<value>]`, with `--toggle`/`--clear` where they apply"). Then delete this file and its `docs/plans/README.md` entry, unwrapping that README's hard-wrapped tail while there.

The PR is titled `feat(cli)!: …`, since the squash merge lands the title on `main`.
