# Configuration Files

Most setup happens in the app: you add TVs with the pairing wizard and edit profiles on the **Profiles** page, and TisVlay saves the result for you. This page is for the cases where you want the files themselves: a reproducible or headless setup, editing by hand, or looking up the exact schema.

TisVlay is configured with TOML, spread across a few files:

| File | Owner | Purpose |
|------|-------|---------|
| `/var/lib/tisvlay/daemon.toml` | the daemon (`tisvlay` user) | TV definitions, discovery, and **system** profiles, the daemon's own persisted store |
| `~/.config/tisvlay/session.toml` | each desktop user | **user** profiles and per-user TV profile assignments |
| `~/.config/tisvlay/tray.toml` | each desktop user | tray notification preferences |

Each service rewrites its own file as state changes, so changes you make in the app, the tray, or the command line are persisted without any file editing. You can also [edit a file by hand and reload it](#hand-editing-and-reloading).

To declare TVs **declaratively** (for a reproducible or headless setup), give the daemon a **seed file** with `tisvlay service daemon --seed <file>`; on NixOS, the module's `settings` option writes it for you. The seed is re-applied on every start, but only for TVs whose `id` is absent from `daemon.toml`, so runtime edits stick, and a still-declared TV that you removed at runtime comes back (drop the seed entry to drop it). Omit an entry's `id` and it is derived deterministically from the entry's content.

## TV definitions

Each TV is a `[[tv]]` entry with a `[tv.config]` vendor block:

```toml
version = 1

[[tv]]
id = "9a3f1b22-c4d8-4e90-b21f-7f1a2c8e5b40"   # assigned at pair time (optional in a seed file)
name = "Office"
vendor = "lg"
primary_input = "HDMI_1"   # the input this PC uses (for input-aware automation)
managed = true             # participate in automation

[tv.profile]
system = "…"               # assigned system profile, by id (see `tisvlay profile system list`)

[tv.config]
ip = "192.168.1.170"
mac = "84:90:0a:63:a9:94"  # Wake-on-LAN; read from the TV at pair time if left out
# LG also stores, set automatically during pairing:
#   client_key = "…"   scheme = "wss"
```

A CEC TV uses the same shape with CEC keys instead:

```toml
[[tv]]
name = "Living Room"
vendor = "cec"

[tv.config]
device = "/dev/cec0"            # the kernel CEC node the TV is reachable on
physical_address = "1.0.0.0"    # the adapter's HDMI port (USB adapters only; ignored on GPU DP-CEC)
emulate_screen_via_power = true # honor screen_off/screen_on by powering the TV (CEC can't blank a live panel)
advertise_osd_name = true       # answer the TV's name poll, so it labels this PC's HDMI input
osd_name = "TisVlay"            # the label the TV shows for the input (14 characters max)
```

In a **seed file** the vendor block can also be written inline: `config = { ip = "192.168.1.170", mac = "84:90:0a:63:a9:94" }`.

With `emulate_screen_via_power` (default `true`), the daemon fulfills `screen_off`/`screen_on` on a CEC TV by powering it off/on, since CEC can't blank a panel that stays on; set it `false` to have the TV ignore screen actions instead.

With `advertise_osd_name` (default `true`), tisvlay answers the TV's name poll on the CEC bus, so the TV labels the PC's HDMI input `osd_name` in its input picker, the way a Chromecast names its own input. Set it `false` to stay silent and keep the TV's generic input label.

## Discovery

```toml
[discovery]
bind_port = 1901           # SSDP source port (0 = OS-chosen ephemeral)
poll_interval_ms = 8000    # streaming-discovery loop period
```

[Firewall & Discovery](../operations/firewall-discovery.md#open-the-pinned-source-port) explains the source port and its other ways to set it.

## Profiles

A profile maps **events** to **actions**. There are two kinds: **system** profiles (in `daemon.toml`) for machine lifecycle and the login screen, and **user** profiles (in `session.toml`) for your desktop session. What each event means, how profiles inherit from one another, and which TVs they act on is explained in [Automation](./automation.md); this section is the file schema.

Any field may be omitted from a profile, in which case it inherits (see [How profiles resolve](./automation.md#how-profiles-resolve)). Setting an event replaces its whole value, so re-gating an inherited action means restating the action too (the app does this for you).

### Action vocabulary

Every event is one of five actions: `none`, `screen_off`, `screen_on`, `power_off`, and `power_on`; the [Automation page](./automation.md#changing-what-happens) says what each does. In a file, an event value is either the bare action token, or a **table** that adds modifiers:

```toml
on_session_unlock = { action = "screen_on", restore_input = true, while_remote = false }
```

- `restore_input` (default `false`): after an on-ish action completes, switch the TV to its `primary_input`. Needs a backend that can switch inputs (LG; not CEC).
- `while_remote` (default `false`): `true` lets the action fire even while a remote session has the TV armed; see [Remote-Session Awareness](./remote-sessions.md#what-it-does-when-armed). The stock defaults below set it on the blanks and power-offs, so overriding one of those with a bare action drops it; write the table form to keep it.

### System profile

Every field with its stock default (generated from the code, so it's always current):

{{#include ../generated/profile-defaults-system.md}}

`automate_when` gates automation on the TV's current input; see [Which TVs react](./automation.md#which-tvs-react).

### User profile

{{#include ../generated/profile-defaults-user.md}}

The two idle timers set **how long** to wait in each lock state; the shared `on_idle_timeout`/`on_idle_resume` actions decide **what** happens. A `0` timer disables idle for that lock state.

## Hand-editing and reloading

Any of the three files can be edited by hand while the services run. Then apply the edit with one of:

- `tisvlay reload`, which reloads the daemon's, the session's and the tray's files, one line per service (a service that isn't running is reported, not failed).
- The app's **Reload** button, in the sidebar, which does the same for all three files.
- `systemctl reload tisvlay-daemon`, which reloads the daemon's file only. `systemctl` reports success as soon as the signal is sent, so the outcome is in the daemon's journal.

How a reload treats your edit:

- **Only the edit applies.** The service compares the file with what it last wrote and applies just the difference, so changes made through the CLI or GUI but not yet written survive. Where both changed the same field, the file wins.
- **A bad edit is refused whole**, for example a missing or unparsable file, a repeated id, a changed or unknown vendor, renaming or removing the `default` profile, or deleting a profile a TV still uses. A parse error points at its file, line and column. A TV in the middle of pairing is left as it is.
- **Comments survive.** The services' own writes keep your comments and layout wherever the values beside them didn't change.
- **Reload before the service writes again.** An edit nobody reloaded is moved aside to `<file>.edited` (one slot, readable only by its owner) with a warning, never silently overwritten; redo the edit in the current file and reload.
