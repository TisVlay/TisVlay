# Automation

TisVlay can react to what your computer is doing: blank the TV when you lock your screen, light it when you come back, power it off when the machine sleeps. You decide what happens by editing **profiles**, which map **events** (lock, unlock, idle, sleep, …) to **actions** (screen off, screen on, power off, power on, or nothing).

This page covers what happens out of the box, how to change it, and the rules that decide which TVs react. For the TV acting on your PC, see [TV Controls PC](./tv-controls-pc.md).

## What happens by default

Out of the box, a [managed](#which-tvs-react) TV follows your desktop:

| When you… | The TV… |
|-----------|---------|
| Lock your screen | blanks (stays powered, so it resumes instantly) |
| Touch the mouse or keyboard at the lock screen | lights up, before you've typed your password |
| Unlock | is already lit, and switches back to your PC's input |
| Suspend the machine | powers off |
| Resume the machine | powers on, and switches back to your PC's input |
| Shut down | powers off |
| Boot | powers on |

The TV blanking or waking along with your monitor (DPMS) is on too. Inactivity timers (blank the TV after N minutes idle) are **off** until you set them.

Wake events never fire for a TV while a stream has it [armed](#streams), so streaming in from elsewhere doesn't light an empty room.

## Changing what happens

Open the **Profiles** page. There are two lists on the left:

- **System profiles** control what the machine does: boot, shutdown, reboot, sleep, wake, and your display blanking (DPMS). They also cover the login screen.
- **User profiles** control what *you* do in your desktop session: lock, unlock, idle, and streams. Each user account has its own.

Every list starts with a **default** profile (marked ✓), which every TV uses until you assign something else. It can't be renamed or deleted.

![The Profiles page, editing a user profile](../docs/screenshots/profiles_pane_user.png)

To change a behavior, select a profile and change the dropdown beside the event, for example **On session lock** from `screen_off` to `none`. Changes apply immediately.

The actions are:

| Action | Effect |
|--------|--------|
| `none` | do nothing |
| `screen_off` | blank the panel; the TV stays powered |
| `screen_on` | unblank |
| `power_off` | put the TV in standby |
| `power_on` | wake the TV from standby |

Next to each dropdown are two small icon toggles:

- **Land on primary input** (the remote icon): after this action, switch the TV to your PC's input. This needs the TV's *Primary* input set on its page, and a TV that can switch inputs (not HDMI-CEC).
- **Suppressed / fires while streaming** (the network icon): whether this event still happens while a stream has the TV armed. See [Streaming](./streaming.md).

The idle events have two timers, **Idle timeout (locked)** and **Idle timeout (unlocked)**, each in seconds, where `0` turns the timer off. The first applies at the lock screen, the second while you're logged in and active. The **On idle timeout** and **On idle resume** actions say what happens when either timer fires and when activity returns.

### Creating, duplicating, and assigning profiles

- Click **+** beside **System profiles** or **User profiles** to create a profile. Duplicate one with the copy icon, and rename or delete it with the icons in its header. The header's **Used by** list links to the TVs that use it.
- A profile only needs to hold what's different. A field you haven't changed is **inheriting** (it follows the default profile); a field you've changed is **set**, and has an accent bar on its left edge and a reset icon. Click reset to hand the field back to the default, or use the reset icon beside **User profile** or **System profile** to do it for every field.
- To use a profile, open the TV on the **TVs** page and pick it under **PROFILES** (**System** and **User**). The pencil beside each dropdown jumps to that profile in the editor. The tray menu also has a **Profiles** submenu for each TV.

![Editing a system profile](../docs/screenshots/profiles_pane_system.png)

### Two ideas that cover most setups

- **A TV that ignores your desktop** (a gaming TV, say): create a user profile with every event set to `none`, and assign it to that TV. Or leave the profile alone and set the system profile's **Automate when** setting so it only automates while the TV is showing your PC (see [below](#which-tvs-react)).
- **Different behavior for different people**: user profiles belong to each account, so your account can blank aggressively while a shared account does nothing, on the same TV.

## The events

### Your desktop session (user profiles)

| Event | Fires when |
|-------|------------|
| On session lock | you lock your screen |
| On lock wake | you touch the mouse or keyboard at the lock screen |
| On session unlock | you unlock |
| On idle timeout | either idle timer runs out |
| On idle resume | activity returns after an unlocked idle |
| Stream start / end | a stream starts or ends |
| On local return | you first touch the PC after a stream ended |

- **Lock wake** is there so the TV is lit *before* you type your password; the later unlock then has nothing left to do. (Wayland sessions only.)
- **Idle** is independent of your desktop's own screen-locking and blanking timers.
- **Multiple users**: only the foreground session on a seat acts. With fast user switching, a backgrounded user's lock and idle events are ignored until they're active again, so one account can't blank or wake the TV under whoever is using the machine.

### The machine (system profiles)

On startup, on shutdown, on reboot, on sleep, and on wake are handled by the system service, with no one logged in. **On DPMS off** and **On DPMS on** mirror your host display's power state onto the TV that shares its connector, so the TV follows your monitor blanking even before you log in. When a session is running, the lock and idle events can fire for the same blank; that overlap is harmless.

### Login-screen idle

An unattended login screen never blanks by itself. The optional **TisVlay greeter** client closes that gap, using the system profile's **Idle timeout** with **On idle timeout** and **On idle resume**. Blanking stays off until you set a non-zero timeout. GDM and plasma-login-manager are supported; the distro packages enable it for GDM, and on NixOS it turns on by itself for either. See [Installation](../getting-started/installation.md#nixos-flake-module). It also lights the panel once at startup, so the login screen isn't left dark after a logout.

## Which TVs react

For an event to do anything to a TV, three things must agree:

1. The TV's **Managed** switch (on its page, under STATUS) is on. An unmanaged TV is never automated, and it doesn't act on your PC either ([TV Controls PC](./tv-controls-pc.md)).
2. The system profile's **Automate when** setting allows it: `always`, `on_primary` (only while the TV is showing your PC's input), or `off_primary` (only while it's showing something else).
3. No stream is suppressing the event (see below).

Manual actions from the app, the tray, or the command line always run, regardless.

| Managed | Automate when | TV on its primary input? | Automated? |
|:-------:|:-------------:|:------------------------:|:----------:|
| no | any | any | no |
| yes | `always` | any | **yes** |
| yes | `on_primary` | yes | **yes** |
| yes | `on_primary` | no | no |
| yes | `on_primary` | unknown (TV off) | **yes** |
| yes | `off_primary` | yes | no |
| yes | `off_primary` | no | **yes** |

### Streams

A separate gate applies on top: while a stream has a TV armed, wake actions are suppressed unless the event's stream toggle is set to fire. It's independent of **Automate when**. The rules are in [Streaming](./streaming.md#what-it-does-when-armed).

## How profiles resolve

Resolution is **per field**, not per profile. For each field, TisVlay looks along a chain and uses the first value that's set:

```text
the TV's assigned profile  →  the default profile  →  built-in defaults
```

So a profile only spells out what it changes, and it keeps tracking the default for everything else. If you later edit the default, every field that isn't set follows along.

User profiles resolve along the same chain, with your user profiles in place of the system ones.

To see exactly what a TV will do, use the command line's `tisvlay system-profile <tv> --effective` (and `user-profile` for your user profile); the app shows set versus inheriting per field.

## Making sure it's running

Lock and idle events need the per-user session service. The sidebar's **Session** indicator should be green. If an event fires but the TV does nothing, see [Automation does nothing](../operations/troubleshooting.md#automation-does-nothing). Actions are verified and retried, and a boot-time or resume-time wake waits for the network; see [Networking & Reliability](../operations/networking-reliability.md).

<details>
<summary>Command-line equivalents</summary>

```sh
tisvlay profile system list                  # system profiles, with ids
tisvlay profile system create "guest" --body '{"on_startup":"none","on_wake":"none"}'
tisvlay system-profile <tv-id> <profile-id>  # assign; --clear unassigns, no id prints it
tisvlay profile user list                    # user profiles
tisvlay user-profile <tv-id> <profile-id>
tisvlay system-profile <tv-id> --effective   # one TV's fully resolved mapping
tisvlay managed <tv-id> off                  # stop automating a TV
```

`tisvlay profile system defaults` and `tisvlay profile user defaults` print the stock mappings. The full schema, with every field and its stock default, is in [Configuration](./configuration.md#profiles); the verbs are in the [CLI Reference](./cli.md).

</details>
