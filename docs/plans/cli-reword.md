# CLI rework: one grammar for TV attributes, profiles split by scope

The `tisvlay` CLI grew a verb at a time, and it shows: one attribute takes a `set-` prefix (`set-volume`) while the rest are bare nouns, toggles take `on|off` in some places and `true|false` in others, reading a value means parsing `list --json`, and profile assignment is split between a top-level `assign` (system) and `user-profile assign` (user). This plan settles one grammar and applies it to the whole tree in a single breaking release.

No compatibility aliases: old names are replaced outright, per the project's no-shims rule. The CLI is not a wire surface (no key, payload or rpc changes), but scripts call it, so the change ships as a breaking `feat(cli)!` commit and gets a release-notes entry.

## The grammar

**Getter/setter.** Every per-TV attribute is one verb, `<attribute> <tv> [<value>]`: with a value it sets, without one it prints the current value. The verbs: `power`, `screen`, `mute`, `managed`, `volume`, `backlight`, `input`, `name`, `config`, `system-profile`, `user-profile`.

**Flags that replace a value** (each conflicts with a positional value):

- `--toggle` on every on/off attribute (`power`, `screen`, `mute`, `managed`). It reads the current value from the cache, sets the opposite, and prints the new value.
- `--clear` on the attributes that can be unset: `system-profile`, `user-profile`, and `input --primary`. Leaving the value out used to clear these; it now reads.
- `--effective` on `system-profile` and `user-profile`: prints the TV's resolved profile (the assigned profile layered over the stock defaults). It replaces the catalogs' `effective` subcommands.

**On/off values.** One `OnOff` `ValueEnum` with aliases: help and completion show `on|off`, and `true/false`, `yes/no` and `1/0` are accepted as aliases. A typed enum rather than clap's `BoolishValueParser`, so help lists exactly two values and getters print through the same type.

**Getter output.** A getter prints what its setter accepts, so output can be fed straight back in:

| Verb | Prints | `--json` |
|------|--------|----------|
| `power` | `on` / `off` (from `is_on`) | the TV's full power state, e.g. `"active_standby"` |
| `screen`, `mute`, `managed` | `on` / `off` | `true` / `false` |
| `volume`, `backlight` | the level, e.g. `30` | `30` |
| `input` | the current input ID | `"HDMI_2"` |
| `input --primary` | the primary input ID, or nothing when unset | the ID, or `null` |
| `name` | the name | `"Den"` |
| `system-profile`, `user-profile` | the assigned profile UUID, or nothing when unassigned | the UUID, or `null` |
| `--effective`, `config` | `key = value` lines (as today) | the object (as today) |

`--json` prints the bare value, not the state object it comes from: `list --json` and `watch` already give the whole objects.

**Missing and stale values.** A getter reads the publisher caches, the way `list` does. Keys persist across a disconnect (the world deletes a TV's keys only when the TV is removed), so the cases are:

- Unknown TV (no `meta/identity`): error, non-zero exit.
- The TV has never reported this value (e.g. no `state/volume` yet): error naming the value, non-zero exit.
- The value is device state (`state/*`) and `state/link` says disconnected: print the last value, warn on stderr that the TV is disconnected and the value may be stale, exit 0.
- Daemon-owned facts (`meta/*`: `managed`, `name`, `input --primary`, `system-profile`) and session-owned ones (`user-profile`) are never stale, so they get no warning. An unset fact (no primary input, no assigned profile) is a valid value: prints nothing (`--json`: `null`), exit 0.

The cache read drains one subscription on the TV's keys until every key the verb needs has arrived, or the quiet window expires. That's a single combined predicate, per the zenoh ordering law.

**`--toggle` on a disconnected TV.** `power --toggle` on a disconnected TV powers it on: a network TV drops its link in standby, so the cached `is_on` is the least trustworthy then, and the daemon already treats a disconnected wake-capable TV as a power-on target. `screen` and `mute` toggles on a disconnected TV fail (there is no link to act on). `managed` is a daemon setting and toggles regardless of the link.

## The command tree

Old → new:

| Today | After |
|-------|-------|
| `set-volume <tv> <level>` | `volume <tv> [<level>]` |
| `backlight <tv> <level>` | `backlight <tv> [<level>]` |
| `power <tv> on\|off` | `power <tv> [on\|off \| --toggle]` |
| `screen <tv> on\|off` | `screen <tv> [on\|off \| --toggle]` |
| `mute <tv> true\|false` | `mute <tv> [on\|off \| --toggle]` |
| `managed <tv> true\|false` | `managed <tv> [on\|off \| --toggle]` |
| `input <tv> <input>` | `input <tv> [<input>]` |
| `primary-input <tv> [<input>]` | `input <tv> --primary [<input> \| --clear]` |
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
| `add <name> --address … [--unmanaged]` | removed: `pair <name> --vendor mock --address …` |
| `pair …` | `pair … [--unmanaged]` |

Unchanged: `status`, `reload`, `enable-access`, `list`, `watch`, `repair` (it reads well next to `remove`), `reconnect`, `remove`, `remote-session {start,stop,status}`.

The result, grouped as `--help` will list it:

```
tisvlay [--socket P] [--user-socket P] [--site S] [--json] <command>

  status | reload | enable-access | list | watch

  discover [--vendor V] [--timeout 3s | --watch]
  pair <name> [--vendor V] (--params JSON | --address A) [--unmanaged]
  repair <tv> | reconnect <tv> | remove <tv>
  vendor [<vendor>]

  power | screen | mute | managed  <tv> [on|off | --toggle]
  volume | backlight               <tv> [<0-100>]
  input                            <tv> [--primary] [<input> | --clear]
  name                             <tv> [<name>]
  config                           <tv> [<patch>]
  system-profile | user-profile    <tv> [<profile> | --clear | --effective]

  profile system | user  {list, create, update, duplicate, delete, defaults}
  remote-session         {start, stop, status}
```

TV attributes stay flat at the top level: they're the most-typed verbs, and a `tv` group would lengthen every command. `profile` gains a scope level because system and user profiles are different schemas (`SystemProfileConfig` holds the power-event policy; `UserProfileConfig` holds lock, idle and remote-session behavior), not one schema at two scopes, so a `--user` flag that silently changes what `--body` means would mislead. The split mirrors the GUI: the Profiles page lists **SYSTEM PROFILES** and **USER PROFILES** as separate sections, and assignment lives on the TV page's **PROFILES** section as one dropdown per scope, which `system-profile <tv>` / `user-profile <tv>` mirror.

## Implementation

All in `tisvlay-cli`; no protocol, daemon or session changes. `pair --unmanaged` rides the existing `managed` field of the pair request (`rpc::AddTv`), which the CLI hard-codes to `true` today.

- **`lib.rs` (the clap tree).** Rework `Command` per the table. `Profile` takes a `ProfileScope` subcommand (`System(SystemProfileCmd)`, `User(UserProfileCmd)`); the two catalog enums lose `Effective` (and the user one `Assign`). Add the `OnOff` value enum. Attribute verbs take `Option<value>` plus their flags, with clap `conflicts_with` / `requires` doing the exclusion (`--clear` requires `--primary` on `input`; `--timeout` conflicts with `--watch`), so no hand-rolled argument validation. Help text and completion helper docs that name old verbs (`assign`, `schema`, `user-profile`) are updated.
- **`--timeout` duration.** Parsed with `humantime::parse_duration` (`humantime` is already in the tree via zenoh; it becomes a direct dependency) and sent as the request's `timeout_ms`.
- **`main.rs` dispatch.**
  - A `read_tv(session, site, tv, paths)` helper hydrates the TV's keys with the combined-predicate drain above, and returns the decoded payloads plus the link state. Every getter and every `--toggle` goes through it; the missing/stale rules live in one place.
  - The user-socket open (today copied into the `UserProfile` and `RemoteSession` arms) becomes one `open_user` helper, used by `profile user`, `user-profile <tv>` and `remote-session`.
  - The daemon release check skips the verbs that only talk to the session service (`profile user`, `user-profile <tv>`, `remote-session`), which check the session service's release instead, as today.
  - `vendor` merges the `vendors` and `schema` arms: no argument prints the catalog, an argument prints that vendor's row and pairing fields (`--json`: the catalog entry with a `schema` array). An unknown vendor is an error.
  - `discover --watch` is today's `discover_session`, unchanged.
  - `add` and `address_config`'s separate use go; `pairing_config` keeps the `--address` shorthand.
  - The pairing link-MAC hint (`link_mac_line`) prints `tisvlay config <id> '{"mac":…}'`.
- **Setter output** stays `ok` (see Open questions).

## Tests

- **Integration (`tests/it/workflows.rs`)**, against the mock stack already booted there: update every existing invocation to the new names, then add a set-then-get round trip per attribute; `--toggle` flipping and printing the new value; `--clear` on both profile scopes and `input --primary`; `--json` printing bare values (and the full power state for `power`); a never-reported value and an unknown TV both exiting non-zero; the stale warning on stderr for a disconnected TV (the mock's test handle can drop the link); `system-profile`/`user-profile --effective`; `vendor` with and without an ID; `pair --unmanaged` landing `managed = off`.
- **Parser (`lib.rs` tests)**, next to `clap_definition_is_consistent`: `OnOff` accepts every alias and rejects other words; the flag exclusions reject `mute <tv> on --toggle`, `input <tv> --clear` without `--primary`, `system-profile <tv> <id> --effective`, and `discover --timeout 1s --watch`.
- `discover --watch` keeps its current coverage (it is the unchanged `discover_session` path).

## Docs and references

The generated CLI reference (`docs/book/src/generated/cli.md`) regenerates with `just docs-gen` in the same commit as the clap change, since `docs-gen-check` gates on drift. Hand-written references to old verbs, swept with `grep -rnE "tisvlay (set-volume|config-set|assign|user-profile|vendors|schema|discover-session|add|rename|primary-input)\b|--timeout-ms"`:

- Book: `getting-started/pairing.md`, `getting-started/quick-start.md`, `guide/automation.md`, `guide/cli.md` (the GUI ↔ CLI table, plus a new short section on the grammar: getters, `--toggle`, `--clear`, exit codes), `guide/configuration.md`, `operations/firewall-discovery.md`, `operations/isolated-networks.md`, `operations/troubleshooting.md` (the mock health check moves to `pair --vendor mock`), `reference/faq.md`.
- `.claude/CLAUDE.md` Quick Reference (`vendors`, `config-set`, `user-profile list`).
- `packaging/nix/test.nix` (the VM test runs `tisvlay … vendors`).
- Ideas: `completion-followups.md` (names `assign` and `schema`), `sdk-cli-only-rpcs.md` (names the CLI's `add` and `user-profile effective`; `add-tv` keeps its rpc, which the daemon tests use, but loses its CLI verb), `cli-update-config-shorthand.md` (its `tvs <name|uuid> set` shape predates this grammar; re-point it at `config <tv>` and at the TV-name idea).
- `docs/plans/mdbook-docs-site.md` (names `tisvlay schema`).
- Left alone: `docs/design.md` (the retired ≤ v0.2.0 architecture, kept as history).

## Out of scope

- **TV names as selectors** (`tisvlay volume "Den" 30`): ambiguity, names that look like UUIDs, and renames racing a script make it its own design. Filed as [docs/ideas/cli-tv-name-selectors.md](../ideas/cli-tv-name-selectors.md).
- **`automate_when` as its own verb**: stays reachable through `config <tv> <patch>`.
- **Live input-ID completion for `input <tv> <TAB>`**: completers run before clap parses argv, so they can't see which TV was typed; offering every TV's inputs at once would mislead. Add it to `completion-followups.md`.

## Commits

1. This plan and the TV-name idea, as a baseline.
2. `feat(cli)!: …`: the clap tree, dispatch, tests, regenerated reference and the hand-written docs sweep, together, since the drift gate ties the generated reference to the clap change and a docs-only follow-up would leave `main` documenting verbs that no longer exist.
3. Retire this plan: the grammar section lands in the book's `guide/cli.md` (user-facing) and a one-line convention in `.claude/CLAUDE.md` § Code conventions ("a new per-TV attribute is a getter/setter verb: `<attribute> <tv> [<value>]`, with `--toggle`/`--clear` where they apply"); then delete this file and its README entry.

## Open questions

- **Setter output.** Setters print `ok` today. Silent success is the more conventional Unix shape and would make `--toggle` (which prints the new value) the only writer that prints. Keep `ok`, or go silent?
- **`user-profile <tv>` next to `profile user`.** The per-TV assignment verb and the catalog group read alike. The GUI uses the same two words (the TV page's PROFILES dropdowns, the Profiles page's USER PROFILES section), which argues the names are right, but it's worth a second look before the rename is cut.
- **`power` getter on a disconnected TV.** It prints the last cached state with the stale warning, like every other `state/*` value. A network TV that dropped its link is most likely in standby, so `on` there is probably wrong; printing `off` would be more accurate for LG but wrong for a CEC TV, which stays reachable in standby. The uniform rule is the transport-agnostic one; this confirms it's the intended one.
