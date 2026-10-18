# TV names as CLI selectors

Every per-TV CLI verb takes the TV's UUID. Shell completion hides that interactively (`<TAB>` offers UUIDs annotated with names), but scripts, docs and copy-pasted commands still carry UUIDs. Accepting a TV's name wherever a UUID is accepted (`tisvlay volume "Den" 30`) would read better.

It was left out of the CLI getter/setter rework deliberately, because it opens several questions at once:

- **Ambiguity.** Names aren't unique. Two TVs named "TV" (the vendor default) must fail with a list of the matches, not pick one.
- **Names that look like UUIDs, or like other arguments.** A name that parses as a UUID must not shadow a real TV; the resolution order (UUID first, then exact name, then nothing) has to be fixed and documented.
- **Renames racing scripts.** A script that addresses TVs by name breaks silently when someone renames one in the GUI. UUIDs are stable; names are display text. Docs would have to steer scripts back to UUIDs.
- **Matching rules.** Exact, case-insensitive, or prefix? Prefix matching is convenient interactively and dangerous in scripts.
- **Cost per command.** Resolving a name means hydrating every TV's `meta/identity` before the command runs, where a UUID needs no lookup.

## Path forward

A single resolver in `tisvlay-cli` (a clap `value_parser` can't do it, since resolution needs the daemon): parse as a UUID first; otherwise hydrate identities and accept exactly one case-insensitive exact match, failing with the candidates listed otherwise.

## Trigger conditions

- A user asks for it, or scripted multi-TV setups show up where UUIDs are the friction.
