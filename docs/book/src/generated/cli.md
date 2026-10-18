**Command Overview:**

* [`tisvlay`↴](#tisvlay)
* [`tisvlay enable-access`↴](#tisvlay-enable-access)
* [`tisvlay status`↴](#tisvlay-status)
* [`tisvlay reload`↴](#tisvlay-reload)
* [`tisvlay list`↴](#tisvlay-list)
* [`tisvlay watch`↴](#tisvlay-watch)
* [`tisvlay discover`↴](#tisvlay-discover)
* [`tisvlay pair`↴](#tisvlay-pair)
* [`tisvlay repair`↴](#tisvlay-repair)
* [`tisvlay reconnect`↴](#tisvlay-reconnect)
* [`tisvlay remove`↴](#tisvlay-remove)
* [`tisvlay vendor`↴](#tisvlay-vendor)
* [`tisvlay power`↴](#tisvlay-power)
* [`tisvlay screen`↴](#tisvlay-screen)
* [`tisvlay mute`↴](#tisvlay-mute)
* [`tisvlay managed`↴](#tisvlay-managed)
* [`tisvlay volume`↴](#tisvlay-volume)
* [`tisvlay backlight`↴](#tisvlay-backlight)
* [`tisvlay input`↴](#tisvlay-input)
* [`tisvlay primary-input`↴](#tisvlay-primary-input)
* [`tisvlay name`↴](#tisvlay-name)
* [`tisvlay config`↴](#tisvlay-config)
* [`tisvlay system-profile`↴](#tisvlay-system-profile)
* [`tisvlay user-profile`↴](#tisvlay-user-profile)
* [`tisvlay profile`↴](#tisvlay-profile)
* [`tisvlay profile system`↴](#tisvlay-profile-system)
* [`tisvlay profile system list`↴](#tisvlay-profile-system-list)
* [`tisvlay profile system create`↴](#tisvlay-profile-system-create)
* [`tisvlay profile system update`↴](#tisvlay-profile-system-update)
* [`tisvlay profile system duplicate`↴](#tisvlay-profile-system-duplicate)
* [`tisvlay profile system delete`↴](#tisvlay-profile-system-delete)
* [`tisvlay profile system defaults`↴](#tisvlay-profile-system-defaults)
* [`tisvlay profile user`↴](#tisvlay-profile-user)
* [`tisvlay profile user list`↴](#tisvlay-profile-user-list)
* [`tisvlay profile user create`↴](#tisvlay-profile-user-create)
* [`tisvlay profile user update`↴](#tisvlay-profile-user-update)
* [`tisvlay profile user duplicate`↴](#tisvlay-profile-user-duplicate)
* [`tisvlay profile user delete`↴](#tisvlay-profile-user-delete)
* [`tisvlay profile user defaults`↴](#tisvlay-profile-user-defaults)
* [`tisvlay remote-session`↴](#tisvlay-remote-session)
* [`tisvlay remote-session start`↴](#tisvlay-remote-session-start)
* [`tisvlay remote-session stop`↴](#tisvlay-remote-session-stop)
* [`tisvlay remote-session status`↴](#tisvlay-remote-session-status)

## `tisvlay`

TisVlay TV control

**Usage:** `tisvlay [OPTIONS] <COMMAND>`

A TV setting or state reads when its value is left out and sets when it's given: `tisvlay volume <TV>` prints the volume, `tisvlay volume <TV> 30` sets it. On/off settings take `--toggle`, and settings that can be unset take `--clear`. Commands that change something print nothing on success.

###### **Subcommands:**

* `enable-access` — Grant the current user TV control by adding it to the daemon's socket group (one-click, authenticated via polkit). Log out and back in afterward for it to take effect
* `status` — Probe the daemon's liveliness token and report its release; fails when the daemon is absent or runs a release this CLI can't talk to
* `reload` — Apply hand edits of the state files: the daemon's, the session's and the tray's (what the GUI's Reload button does)
* `list` — List TVs with their current state (hydrates from publisher caches)
* `watch` — Stream every data key: history first, then live samples
* `discover` — Scan for TVs: a bounded window, or with `--watch` until ctrl-c
* `pair` — Pair a new TV, streaming the pairing progress (ctrl-c cancels)
* `repair` — Re-run the pairing handshake for an existing TV
* `reconnect` — Ask a downed TV to reconnect
* `remove` — Remove a TV (deletes all its keys)
* `vendor` — List the vendor backends, or show one's capabilities and pairing fields
* `power` — Power a TV on or off, or print whether it's on (connected and awake)
* `screen` — Blank or wake a TV's screen, or print whether it shows a picture
* `mute` — Mute or unmute a TV, or print whether it's muted
* `managed` — Set whether the daemon automates a TV, or print it
* `volume` — Set a TV's volume, or print it
* `backlight` — Set a TV's backlight level, or print it
* `input` — Switch a TV's input, or print the current one
* `primary-input` — Set a TV's primary input (the one automation restores and compares against), or print it
* `name` — Rename a TV, or print its name
* `config` — Patch a TV's config, or print it (vendor secrets redacted)
* `system-profile` — Assign a TV's system profile, or print the assigned one
* `user-profile` — Assign a TV's user profile (via --user-socket), or print the assigned one
* `profile` — The profile catalogs: system profiles hold what TVs do on power events, user profiles what they do on lock, idle and remote sessions
* `remote-session` — Arm or query a remote-session source (session-service side, via --user-socket). `start`/`stop` are the Sunshine `global_prep_cmd` integration (do = start, undo = stop)

###### **Options:**

* `--socket <SOCKET>` — Daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock, falling back to the packaged /run/tisvlay/zenoh.sock)
* `--user-socket <USER_SOCKET>` — Session-service user socket for the `profile user`, `user-profile` and `remote-session` verbs (default: $XDG_RUNTIME_DIR/tisvlay/session.sock)
* `--site <SITE>` — Site chunk used in all key expressions; leave it unless you run more than one daemon

  Default value: `main`
* `--json` — Machine-readable JSON output on read verbs



## `tisvlay enable-access`

Grant the current user TV control by adding it to the daemon's socket group (one-click, authenticated via polkit). Log out and back in afterward for it to take effect

**Usage:** `tisvlay enable-access`



## `tisvlay status`

Probe the daemon's liveliness token and report its release; fails when the daemon is absent or runs a release this CLI can't talk to

**Usage:** `tisvlay status`



## `tisvlay reload`

Apply hand edits of the state files: the daemon's, the session's and the tray's (what the GUI's Reload button does)

**Usage:** `tisvlay reload`



## `tisvlay list`

List TVs with their current state (hydrates from publisher caches)

**Usage:** `tisvlay list`



## `tisvlay watch`

Stream every data key: history first, then live samples

**Usage:** `tisvlay watch`



## `tisvlay discover`

Scan for TVs: a bounded window, or with `--watch` until ctrl-c

**Usage:** `tisvlay discover [OPTIONS]`

###### **Options:**

* `--vendor <VENDOR>`
* `--timeout <TIMEOUT>` — How long to scan, e.g. `3s` or `500ms` (default: the daemon's window; it scans for at most its own cap)
* `--watch` — Keep scanning, streaming findings until ctrl-c



## `tisvlay pair`

Pair a new TV, streaming the pairing progress (ctrl-c cancels)

**Usage:** `tisvlay pair [OPTIONS] --params <PARAMS> <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--vendor <VENDOR>`

  Default value: `lg`
* `--params <PARAMS>` — Vendor pairing params as JSON, e.g. '{"ip":"192.168.1.50"}'
* `--unmanaged` — Enroll the TV unmanaged: the daemon won't automate it



## `tisvlay repair`

Re-run the pairing handshake for an existing TV

**Usage:** `tisvlay repair <TV>`

###### **Arguments:**

* `<TV>`



## `tisvlay reconnect`

Ask a downed TV to reconnect

**Usage:** `tisvlay reconnect <TV>`

###### **Arguments:**

* `<TV>`



## `tisvlay remove`

Remove a TV (deletes all its keys)

**Usage:** `tisvlay remove <TV>`

###### **Arguments:**

* `<TV>`



## `tisvlay vendor`

List the vendor backends, or show one's capabilities and pairing fields

**Usage:** `tisvlay vendor [VENDOR]`

###### **Arguments:**

* `<VENDOR>`



## `tisvlay power`

Power a TV on or off, or print whether it's on (connected and awake)

**Usage:** `tisvlay power [OPTIONS] <TV> [VALUE]`

###### **Arguments:**

* `<TV>`
* `<VALUE>` — Turn it on or off; leave it out to print the current value

  Possible values: `on`, `off`


###### **Options:**

* `--toggle` — Flip the current value, printing the one sent



## `tisvlay screen`

Blank or wake a TV's screen, or print whether it shows a picture

**Usage:** `tisvlay screen [OPTIONS] <TV> [VALUE]`

###### **Arguments:**

* `<TV>`
* `<VALUE>` — Turn it on or off; leave it out to print the current value

  Possible values: `on`, `off`


###### **Options:**

* `--toggle` — Flip the current value, printing the one sent



## `tisvlay mute`

Mute or unmute a TV, or print whether it's muted

**Usage:** `tisvlay mute [OPTIONS] <TV> [VALUE]`

###### **Arguments:**

* `<TV>`
* `<VALUE>` — Turn it on or off; leave it out to print the current value

  Possible values: `on`, `off`


###### **Options:**

* `--toggle` — Flip the current value, printing the one sent



## `tisvlay managed`

Set whether the daemon automates a TV, or print it

**Usage:** `tisvlay managed [OPTIONS] <TV> [VALUE]`

###### **Arguments:**

* `<TV>`
* `<VALUE>` — Turn it on or off; leave it out to print the current value

  Possible values: `on`, `off`


###### **Options:**

* `--toggle` — Flip the current value, printing the one sent



## `tisvlay volume`

Set a TV's volume, or print it

**Usage:** `tisvlay volume <TV> [LEVEL]`

###### **Arguments:**

* `<TV>`
* `<LEVEL>` — 0–100; leave it out to print the current level



## `tisvlay backlight`

Set a TV's backlight level, or print it

**Usage:** `tisvlay backlight <TV> [LEVEL]`

###### **Arguments:**

* `<TV>`
* `<LEVEL>` — 0–100; leave it out to print the current level



## `tisvlay input`

Switch a TV's input, or print the current one

**Usage:** `tisvlay input <TV> [INPUT]`

###### **Arguments:**

* `<TV>`
* `<INPUT>` — The input ID; leave it out to print the current one



## `tisvlay primary-input`

Set a TV's primary input (the one automation restores and compares against), or print it

**Usage:** `tisvlay primary-input [OPTIONS] <TV> [INPUT]`

###### **Arguments:**

* `<TV>`
* `<INPUT>` — The input ID; leave it out to print the primary one

###### **Options:**

* `--clear` — Unset the primary input



## `tisvlay name`

Rename a TV, or print its name

**Usage:** `tisvlay name <TV> [NAME]`

###### **Arguments:**

* `<TV>`
* `<NAME>` — The new name; leave it out to print the current one



## `tisvlay config`

Patch a TV's config, or print it (vendor secrets redacted)

**Usage:** `tisvlay config <TV> [PATCH]`

###### **Arguments:**

* `<TV>`
* `<PATCH>` — A JSON object merged into the config (null unsets a vendor key); leave it out to print the config



## `tisvlay system-profile`

Assign a TV's system profile, or print the assigned one

**Usage:** `tisvlay system-profile [OPTIONS] <TV> [PROFILE]`

###### **Arguments:**

* `<TV>`
* `<PROFILE>` — The profile to assign; leave it out to print the assigned one (the default profile's UUID when none is)

###### **Options:**

* `--clear` — Unassign, so the TV follows the default profile
* `--effective` — Print the TV's resolved profile: the assigned one over the stock defaults



## `tisvlay user-profile`

Assign a TV's user profile (via --user-socket), or print the assigned one

**Usage:** `tisvlay user-profile [OPTIONS] <TV> [PROFILE]`

###### **Arguments:**

* `<TV>`
* `<PROFILE>` — The profile to assign; leave it out to print the assigned one (the default profile's UUID when none is)

###### **Options:**

* `--clear` — Unassign, so the TV follows the default profile
* `--effective` — Print the TV's resolved profile: the assigned one over the stock defaults



## `tisvlay profile`

The profile catalogs: system profiles hold what TVs do on power events, user profiles what they do on lock, idle and remote sessions

**Usage:** `tisvlay profile <COMMAND>`

###### **Subcommands:**

* `system` — System profiles (daemon-side)
* `user` — User profiles (session-service side, via --user-socket)



## `tisvlay profile system`

System profiles (daemon-side)

**Usage:** `tisvlay profile system <COMMAND>`

###### **Subcommands:**

* `list` — List profile bodies from the catalog caches
* `create` — Create a profile, printing its UUID: `--body` is a SystemProfileConfig body
* `update` — Replace a profile's body (full JSON body; name required inside)
* `duplicate` — Copy a profile under a new name, printing the copy's UUID
* `delete` — Delete a profile (assignments referring to it fall back to default)
* `defaults` — The stock mapping unset fields inherit



## `tisvlay profile system list`

List profile bodies from the catalog caches

**Usage:** `tisvlay profile system list`



## `tisvlay profile system create`

Create a profile, printing its UUID: `--body` is a SystemProfileConfig body

**Usage:** `tisvlay profile system create [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--body <BODY>`



## `tisvlay profile system update`

Replace a profile's body (full JSON body; name required inside)

**Usage:** `tisvlay profile system update <ID> <BODY>`

###### **Arguments:**

* `<ID>`
* `<BODY>`



## `tisvlay profile system duplicate`

Copy a profile under a new name, printing the copy's UUID

**Usage:** `tisvlay profile system duplicate <ID> <NAME>`

###### **Arguments:**

* `<ID>`
* `<NAME>`



## `tisvlay profile system delete`

Delete a profile (assignments referring to it fall back to default)

**Usage:** `tisvlay profile system delete <ID>`

###### **Arguments:**

* `<ID>`



## `tisvlay profile system defaults`

The stock mapping unset fields inherit

**Usage:** `tisvlay profile system defaults`



## `tisvlay profile user`

User profiles (session-service side, via --user-socket)

**Usage:** `tisvlay profile user <COMMAND>`

###### **Subcommands:**

* `list` — List profile bodies from the catalog caches
* `create` — Create a profile, printing its UUID: `--body` is a UserProfileConfig body
* `update` — Replace a profile's body (full JSON body; name required inside)
* `duplicate` — Copy a profile under a new name, printing the copy's UUID
* `delete` — Delete a profile (assignments referring to it fall back to default)
* `defaults` — The stock mapping unset fields inherit



## `tisvlay profile user list`

List profile bodies from the catalog caches

**Usage:** `tisvlay profile user list`



## `tisvlay profile user create`

Create a profile, printing its UUID: `--body` is a UserProfileConfig body

**Usage:** `tisvlay profile user create [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--body <BODY>`



## `tisvlay profile user update`

Replace a profile's body (full JSON body; name required inside)

**Usage:** `tisvlay profile user update <ID> <BODY>`

###### **Arguments:**

* `<ID>`
* `<BODY>`



## `tisvlay profile user duplicate`

Copy a profile under a new name, printing the copy's UUID

**Usage:** `tisvlay profile user duplicate <ID> <NAME>`

###### **Arguments:**

* `<ID>`
* `<NAME>`



## `tisvlay profile user delete`

Delete a profile (assignments referring to it fall back to default)

**Usage:** `tisvlay profile user delete <ID>`

###### **Arguments:**

* `<ID>`



## `tisvlay profile user defaults`

The stock mapping unset fields inherit

**Usage:** `tisvlay profile user defaults`



## `tisvlay remote-session`

Arm or query a remote-session source (session-service side, via --user-socket). `start`/`stop` are the Sunshine `global_prep_cmd` integration (do = start, undo = stop)

**Usage:** `tisvlay remote-session <COMMAND>`

###### **Subcommands:**

* `start` — Arm the named source
* `stop` — Disarm the named source
* `status` — Show the current remote-session fact (live sources + armed TVs)



## `tisvlay remote-session start`

Arm the named source

**Usage:** `tisvlay remote-session start [OPTIONS]`

###### **Options:**

* `--source <SOURCE>`

  Default value: `manual`



## `tisvlay remote-session stop`

Disarm the named source

**Usage:** `tisvlay remote-session stop [OPTIONS]`

###### **Options:**

* `--source <SOURCE>`

  Default value: `manual`



## `tisvlay remote-session status`

Show the current remote-session fact (live sources + armed TVs)

**Usage:** `tisvlay remote-session status`
