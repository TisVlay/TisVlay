# Profiles

Profiles map **events** to **actions** — they are the entire automation vocabulary. The schema, action table, and field lists live in [Configuration](./configuration.md#profiles); this page covers how profiles *resolve* and the patterns that fall out of that.

## The reserved default profiles

Every install has one reserved profile per kind, both named `default` (system profile UUID ending `…5159`, user profile UUID ending `…515a`). They are special in three ways:

- A TV with no explicit assignment uses the default — so the defaults alone already give working automation.
- Every other profile inherits from them.
- They can't be deleted or renamed (an unassigned TV implicitly references the default, so deleting it would orphan TVs).

## Resolution order

Resolution is **per field**, not per profile. Each field is looked up along a chain and the first explicit value wins:

```text
TV's assigned profile  →  the default profile  →  built-in defaults
```

A profile therefore only needs to spell out what it changes. For example, a system profile for a guest-room TV that should never turn on by itself:

```toml
[system.profiles.3c8d0a51-9b7e-4f2d-8e6a-1b5c9d3f7a20]
name = "guest"
on_startup = "none"
on_shutdown = "screen_off"
```

Every other field (`on_sleep`, `on_wake`, …) flows through from `default`, and tracks `default` if you later edit it — only the two overridden fields are pinned.

For **user** profiles the chain is one layer longer: your profile → your `default` → the admin-set baseline in the daemon config (a `[user.profiles]` section there) → built-ins. On a single-user machine you can ignore the admin layer.

## Assigning profiles to TVs

Each TV carries up to two assignments: `profile.system` (in the daemon config) and `profile.user` (in your per-user config). Set them with:

- **CLI** — `tisvlay profile system set <uuid> -t <tv-id>` / `tisvlay profile user set <uuid>`; `… list` shows the catalogs with UUIDs.
- **Tray** — the per-TV *Profile* submenu (radio groups for user and system).
- **GUI** — the TVs pane detail view.

## Creating and editing profiles

The **GUI Profiles pane** is the full editor: create, rename, duplicate, and delete profiles, with a per-field editor where each field is either *set* on this profile or *inheriting* — a reset control flips a field back to inherited. See [GUI & Tray](./gui-tray.md).

Alternatively hand-edit the config files and call `ReloadConfig` ([Configuration](./configuration.md)). The CLI manages *assignments*, not profile bodies.

## The automation gate

Whether an event acts on a TV at all composes `managed`, the profile's `automate_when`, and the TV's current input:

| `managed` | `automate_when` | TV on `primary_input`? | automated? |
|:---------:|:---------------:|:----------------------:|:----------:|
| false | any | any | no |
| true | `always` | any | **yes** |
| true | `on_primary` | yes | **yes** |
| true | `on_primary` | no | no |
| true | `on_primary` | unknown (TV off) | **yes** |
| true | `off_primary` | yes | no |
| true | `off_primary` | no | **yes** |

Manual actions — tray, GUI, CLI — always execute regardless of the gate.

## Patterns

- **Gaming TV that ignores the desktop**: a user profile with `on_session_lock = "none"` and `on_idle_timeout = "none"` assigned to that TV — or simply `automate_when = "on_primary"` in its system profile, so it automates only while showing the PC.
- **Different users, different behavior**: user profiles live per account, so your account can blank aggressively while a shared account does nothing — same TV, same daemon.
