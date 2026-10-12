# Profiles

Profiles map **events** to **actions** — they are the entire automation vocabulary. The schema, action table, and field lists live in [Configuration](./configuration.md#profiles); this page covers how profiles *resolve* and the patterns that fall out of that.

## The reserved default profiles

Every install has one reserved profile per kind, both named `default`. They are special in three ways:

- A TV with no explicit assignment uses the default — so the defaults alone already give working automation.
- Every other profile inherits from them.
- They can't be deleted or renamed (an unassigned TV implicitly references the default, so deleting it would orphan TVs).

## Resolution order

Resolution is **per field**, not per profile. Each field is looked up along a chain and the first explicit value wins:

```text
TV's assigned profile  →  the default profile  →  built-in defaults
```

A profile therefore only needs to spell out what it changes. For example, a system profile for a guest-room TV that should never turn on by itself:

```sh
tisvlay profile create "guest" --body '{"on_startup":"none","on_wake":"none"}'
```

Every other field (`on_shutdown`, `on_sleep`, …) flows through from `default`, and tracks `default` if you later edit it — only the two overridden fields are pinned.

For **user** profiles the chain is one layer longer: your profile → your `default` → an admin-set baseline (a system-wide user-profile default) → built-ins. On a single-user machine you can ignore the admin layer.

## Assigning profiles to TVs

Each TV carries up to two assignments: a **system** profile (in the daemon store) and a **user** profile (in your per-user config). Set them with:

- **CLI** — `tisvlay assign <tv-id> <profile-id>` (system) and `tisvlay user-profile assign <tv-id> <profile-id>` (user); pass no profile id to clear the assignment back to the default. `tisvlay profile list` / `tisvlay user-profile list` show the catalogs with their ids.
- **Tray** — the per-TV *Profiles* submenu (radio groups for user and system).
- **GUI** — the TVs pane detail view.

## Creating and editing profiles

The **GUI Profiles pane** is the full editor: create, rename, duplicate, and delete profiles, with a per-field editor where each field is either *set* on this profile or *inheriting* — a reset control flips a field back to inherited. See [GUI & Tray](./gui-tray.md).

The CLI manages both bodies and assignments: `tisvlay profile create|update|duplicate|delete` (and `tisvlay user-profile …` for user profiles), `tisvlay profile defaults` for the stock mapping, and `tisvlay profile effective <tv>` to print one TV's fully-resolved mapping.

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

## The remote-session gate

A second, independent gate rides on top: while a remote session has a TV armed, wake actions are suppressed unless the event sets `while_remote = true`. It applies after the automation gate above and is independent of `automate_when`; [Remote-Session Awareness](./remote-sessions.md#what-it-does-when-armed) has the rules.

## Patterns

- **Gaming TV that ignores the desktop**: a user profile with `on_session_lock = "none"` and the idle actions set to `none`, assigned to that TV — or simply `automate_when = "on_primary"` in its system profile, so it automates only while showing the PC.
- **Different users, different behavior**: user profiles live per account, so your account can blank aggressively while a shared account does nothing — same TV, same daemon.
