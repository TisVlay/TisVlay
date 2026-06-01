# Command-Line Help for `tisvlay`

This document contains the help content for the `tisvlay` command-line program.

**Command Overview:**

* [`tisvlay`↴](#tisvlay)
* [`tisvlay power`↴](#tisvlay-power)
* [`tisvlay power on`↴](#tisvlay-power-on)
* [`tisvlay power off`↴](#tisvlay-power-off)
* [`tisvlay screen`↴](#tisvlay-screen)
* [`tisvlay screen on`↴](#tisvlay-screen-on)
* [`tisvlay screen off`↴](#tisvlay-screen-off)
* [`tisvlay input`↴](#tisvlay-input)
* [`tisvlay volume`↴](#tisvlay-volume)
* [`tisvlay mute`↴](#tisvlay-mute)
* [`tisvlay backlight`↴](#tisvlay-backlight)
* [`tisvlay status`↴](#tisvlay-status)
* [`tisvlay pair`↴](#tisvlay-pair)
* [`tisvlay re-pair`↴](#tisvlay-re-pair)
* [`tisvlay list`↴](#tisvlay-list)
* [`tisvlay remove`↴](#tisvlay-remove)
* [`tisvlay manage`↴](#tisvlay-manage)
* [`tisvlay profile`↴](#tisvlay-profile)
* [`tisvlay profile user`↴](#tisvlay-profile-user)
* [`tisvlay profile user list`↴](#tisvlay-profile-user-list)
* [`tisvlay profile user get`↴](#tisvlay-profile-user-get)
* [`tisvlay profile user set`↴](#tisvlay-profile-user-set)
* [`tisvlay profile system`↴](#tisvlay-profile-system)
* [`tisvlay profile system list`↴](#tisvlay-profile-system-list)
* [`tisvlay profile system get`↴](#tisvlay-profile-system-get)
* [`tisvlay profile system set`↴](#tisvlay-profile-system-set)

## `tisvlay`

TV Control

**Usage:** `tisvlay [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `power` — Power control
* `screen` — Screen control (blank/unblank for OLED protection)
* `input` — Switch HDMI input (lists available inputs when no ID given)
* `volume` — Volume control
* `mute` — Mute/unmute audio
* `backlight` — Backlight control (OLED pixel brightness)
* `status` — Show TV status
* `pair` — Pair a new TV
* `re-pair` — Re-pair an existing TV (refresh expired credentials in place)
* `list` — List configured TVs
* `remove` — Remove a TV from configuration
* `manage` — Enable or disable automated management for a TV (shows current status if no argument given)
* `profile` — Profile management (requires tisvlay-session to be running)

###### **Options:**

* `-t`, `--tv <TV>` — Target TV ID (default: first configured TV)
* `-v`, `--verbose` — Enable verbose output
* `--user` — Use session bus instead of system D-Bus (e.g. with `tisvlayd --session`)
* `--json` — Output in JSON format (for scripting and tools like Nushell)



## `tisvlay power`

Power control

**Usage:** `tisvlay power <COMMAND>`

###### **Subcommands:**

* `on` — Turn on the TV (Wake-on-LAN)
* `off` — Turn off the TV (standby)



## `tisvlay power on`

Turn on the TV (Wake-on-LAN)

**Usage:** `tisvlay power on`



## `tisvlay power off`

Turn off the TV (standby)

**Usage:** `tisvlay power off`



## `tisvlay screen`

Screen control (blank/unblank for OLED protection)

**Usage:** `tisvlay screen <COMMAND>`

###### **Subcommands:**

* `on` — Turn on the screen
* `off` — Turn off the screen (OLED-safe blank)



## `tisvlay screen on`

Turn on the screen

**Usage:** `tisvlay screen on`



## `tisvlay screen off`

Turn off the screen (OLED-safe blank)

**Usage:** `tisvlay screen off`



## `tisvlay input`

Switch HDMI input (lists available inputs when no ID given)

**Usage:** `tisvlay input [INPUT_ID]`

###### **Arguments:**

* `<INPUT_ID>` — Input ID (e.g., HDMI_1, HDMI_2)



## `tisvlay volume`

Volume control

**Usage:** `tisvlay volume [LEVEL]`

###### **Arguments:**

* `<LEVEL>` — Volume level (0-100), or +/- for relative adjustment



## `tisvlay mute`

Mute/unmute audio

**Usage:** `tisvlay mute [STATE]`

###### **Arguments:**

* `<STATE>`

  Default value: `toggle`

  Possible values: `on`, `off`, `toggle`




## `tisvlay backlight`

Backlight control (OLED pixel brightness)

**Usage:** `tisvlay backlight [LEVEL]`

###### **Arguments:**

* `<LEVEL>` — Backlight level (0-100)



## `tisvlay status`

Show TV status

**Usage:** `tisvlay status`



## `tisvlay pair`

Pair a new TV

**Usage:** `tisvlay pair [OPTIONS] --vendor <VENDOR> <ADDRESS>`

###### **Arguments:**

* `<ADDRESS>` — TV address (e.g., IP address, serial port)

###### **Options:**

* `-n`, `--name <NAME>` — TV name
* `-w`, `--wake-id <WAKE_ID>` — Hardware ID for wake support (e.g., MAC address for WoL)
* `--vendor <VENDOR>` — Vendor backend to use (e.g., lg)
* `--managed <MANAGED>` — Participate in automation (boot WoL, session events, shutdown). Pass --managed false to pair for manual control only

  Default value: `true`

  Possible values: `true`, `false`




## `tisvlay re-pair`

Re-pair an existing TV (refresh expired credentials in place)

**Usage:** `tisvlay re-pair <TV_ID>`

###### **Arguments:**

* `<TV_ID>` — TV ID to re-pair



## `tisvlay list`

List configured TVs

**Usage:** `tisvlay list`



## `tisvlay remove`

Remove a TV from configuration

**Usage:** `tisvlay remove <TV_ID>`

###### **Arguments:**

* `<TV_ID>` — TV ID to remove



## `tisvlay manage`

Enable or disable automated management for a TV (shows current status if no argument given)

**Usage:** `tisvlay manage [STATE]`

###### **Arguments:**

* `<STATE>`

  Possible values: `true`, `false`




## `tisvlay profile`

Profile management (requires tisvlay-session to be running)

**Usage:** `tisvlay profile <COMMAND>`

###### **Subcommands:**

* `user` — User profile management
* `system` — System profile management (daemon lifecycle events)



## `tisvlay profile user`

User profile management

**Usage:** `tisvlay profile user <COMMAND>`

###### **Subcommands:**

* `list` — List all available user profiles
* `get` — Show the user profile assigned to a TV
* `set` — Assign a user profile to a TV



## `tisvlay profile user list`

List all available user profiles

**Usage:** `tisvlay profile user list`



## `tisvlay profile user get`

Show the user profile assigned to a TV

**Usage:** `tisvlay profile user get`



## `tisvlay profile user set`

Assign a user profile to a TV

**Usage:** `tisvlay profile user set <PROFILE_UUID>`

###### **Arguments:**

* `<PROFILE_UUID>` — Profile UUID to assign (string-formatted)



## `tisvlay profile system`

System profile management (daemon lifecycle events)

**Usage:** `tisvlay profile system <COMMAND>`

###### **Subcommands:**

* `list` — List all available system profiles
* `get` — Show the system profile assigned to a TV
* `set` — Assign a system profile to a TV



## `tisvlay profile system list`

List all available system profiles

**Usage:** `tisvlay profile system list`



## `tisvlay profile system get`

Show the system profile assigned to a TV

**Usage:** `tisvlay profile system get`



## `tisvlay profile system set`

Assign a system profile to a TV

**Usage:** `tisvlay profile system set <PROFILE_UUID>`

###### **Arguments:**

* `<PROFILE_UUID>` — Profile UUID to assign (string-formatted)



<hr/>

<small><i>
    This document was generated automatically by
    <a href="https://crates.io/crates/clap-markdown"><code>clap-markdown</code></a>.
</i></small>
