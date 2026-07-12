**Command Overview:**

* [`tisvlay`↴](#tisvlay)
* [`tisvlay status`↴](#tisvlay-status)
* [`tisvlay list`↴](#tisvlay-list)
* [`tisvlay watch`↴](#tisvlay-watch)
* [`tisvlay discover`↴](#tisvlay-discover)
* [`tisvlay pair`↴](#tisvlay-pair)
* [`tisvlay add`↴](#tisvlay-add)
* [`tisvlay set-volume`↴](#tisvlay-set-volume)
* [`tisvlay input`↴](#tisvlay-input)
* [`tisvlay backlight`↴](#tisvlay-backlight)
* [`tisvlay screen`↴](#tisvlay-screen)
* [`tisvlay rename`↴](#tisvlay-rename)
* [`tisvlay managed`↴](#tisvlay-managed)
* [`tisvlay config`↴](#tisvlay-config)
* [`tisvlay config-set`↴](#tisvlay-config-set)
* [`tisvlay repair`↴](#tisvlay-repair)
* [`tisvlay power`↴](#tisvlay-power)
* [`tisvlay reconnect`↴](#tisvlay-reconnect)
* [`tisvlay remove`↴](#tisvlay-remove)
* [`tisvlay mute`↴](#tisvlay-mute)
* [`tisvlay primary-input`↴](#tisvlay-primary-input)
* [`tisvlay assign`↴](#tisvlay-assign)
* [`tisvlay vendors`↴](#tisvlay-vendors)
* [`tisvlay schema`↴](#tisvlay-schema)
* [`tisvlay discover-session`↴](#tisvlay-discover-session)
* [`tisvlay profile`↴](#tisvlay-profile)
* [`tisvlay profile list`↴](#tisvlay-profile-list)
* [`tisvlay profile create`↴](#tisvlay-profile-create)
* [`tisvlay profile update`↴](#tisvlay-profile-update)
* [`tisvlay profile duplicate`↴](#tisvlay-profile-duplicate)
* [`tisvlay profile delete`↴](#tisvlay-profile-delete)
* [`tisvlay profile defaults`↴](#tisvlay-profile-defaults)
* [`tisvlay profile effective`↴](#tisvlay-profile-effective)
* [`tisvlay user-profile`↴](#tisvlay-user-profile)
* [`tisvlay user-profile list`↴](#tisvlay-user-profile-list)
* [`tisvlay user-profile create`↴](#tisvlay-user-profile-create)
* [`tisvlay user-profile update`↴](#tisvlay-user-profile-update)
* [`tisvlay user-profile duplicate`↴](#tisvlay-user-profile-duplicate)
* [`tisvlay user-profile delete`↴](#tisvlay-user-profile-delete)
* [`tisvlay user-profile defaults`↴](#tisvlay-user-profile-defaults)
* [`tisvlay user-profile effective`↴](#tisvlay-user-profile-effective)
* [`tisvlay user-profile assign`↴](#tisvlay-user-profile-assign)
* [`tisvlay remote-session`↴](#tisvlay-remote-session)
* [`tisvlay remote-session start`↴](#tisvlay-remote-session-start)
* [`tisvlay remote-session stop`↴](#tisvlay-remote-session-stop)
* [`tisvlay remote-session status`↴](#tisvlay-remote-session-status)

## `tisvlay`

TisVlay TV control

**Usage:** `tisvlay [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `status` — Probe the daemon's liveliness token
* `list` — List TVs with their current state (hydrates from publisher caches)
* `watch` — Stream every data key: history first, then live samples
* `discover` — Scan for TVs (bounded window; findings stream back as replies)
* `pair` — Pair a new TV, streaming the pairing progress (ctrl-c cancels)
* `add` — Add a mock TV (shortcut around the add-tv rpc)
* `set-volume` — Set a TV's volume
* `input` — Switch a TV's input
* `backlight` — Set a TV's backlight level
* `screen` — Blank or wake the screen
* `rename` — Rename a TV
* `managed` — Toggle whether the daemon drives this TV
* `config` — Read a TV's config (vendor secrets redacted)
* `config-set` — Patch a TV's config with a JSON object (null unsets a vendor key)
* `repair` — Re-run the pairing handshake for an existing TV
* `power` — Power a TV on or off
* `reconnect` — Ask a downed TV to reconnect
* `remove` — Remove a TV (deletes all its keys)
* `mute` — Mute or unmute a TV
* `primary-input` — Set (or with no value clear) the TV's primary input
* `assign` — Assign (or with no value clear) the TV's system profile
* `vendors` — List registered vendor backends with their capabilities
* `schema` — Show a vendor's pairing form schema
* `discover-session` — Open-ended discovery session: findings stream until ctrl-c
* `profile` — System-profile catalog (daemon-side)
* `user-profile` — User-profile catalog (session-service side, via --user-socket)
* `remote-session` — Arm or query a remote-session source (session-service side, via --user-socket). `start`/`stop` are the Sunshine `global_prep_cmd` integration (do = start, undo = stop)

###### **Options:**

* `--socket <SOCKET>` — Daemon socket (default: $XDG_RUNTIME_DIR/tisvlay/zenoh.sock)
* `--user-socket <USER_SOCKET>` — Session-service user socket for `user-profile` verbs (default: $XDG_RUNTIME_DIR/tisvlay/session.sock)
* `--site <SITE>` — Site chunk used in all key expressions

  Default value: `main`
* `--json` — Machine-readable JSON output on read verbs



## `tisvlay status`

Probe the daemon's liveliness token

**Usage:** `tisvlay status`



## `tisvlay list`

List TVs with their current state (hydrates from publisher caches)

**Usage:** `tisvlay list`



## `tisvlay watch`

Stream every data key: history first, then live samples

**Usage:** `tisvlay watch`



## `tisvlay discover`

Scan for TVs (bounded window; findings stream back as replies)

**Usage:** `tisvlay discover [OPTIONS]`

###### **Options:**

* `--vendor <VENDOR>`
* `--timeout-ms <TIMEOUT_MS>`

  Default value: `3000`



## `tisvlay pair`

Pair a new TV, streaming the pairing progress (ctrl-c cancels)

**Usage:** `tisvlay pair [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--vendor <VENDOR>`

  Default value: `lg`
* `--params <PARAMS>` — Vendor pairing params as JSON, e.g. '{"ip":"192.168.1.50"}'
* `--address <ADDRESS>` — Mock shorthand for --params '{"address":"mock://…"}'



## `tisvlay add`

Add a mock TV (shortcut around the add-tv rpc)

**Usage:** `tisvlay add [OPTIONS] --address <ADDRESS> <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--address <ADDRESS>` — Mock vendor address, e.g. mock://den
* `--vendor <VENDOR>`

  Default value: `mock`
* `--unmanaged`



## `tisvlay set-volume`

Set a TV's volume

**Usage:** `tisvlay set-volume <ID> <LEVEL>`

###### **Arguments:**

* `<ID>`
* `<LEVEL>`



## `tisvlay input`

Switch a TV's input

**Usage:** `tisvlay input <ID> <INPUT>`

###### **Arguments:**

* `<ID>`
* `<INPUT>`



## `tisvlay backlight`

Set a TV's backlight level

**Usage:** `tisvlay backlight <ID> <LEVEL>`

###### **Arguments:**

* `<ID>`
* `<LEVEL>`



## `tisvlay screen`

Blank or wake the screen

**Usage:** `tisvlay screen <ID> <STATE>`

###### **Arguments:**

* `<ID>`
* `<STATE>`

  Possible values: `on`, `off`




## `tisvlay rename`

Rename a TV

**Usage:** `tisvlay rename <ID> <NAME>`

###### **Arguments:**

* `<ID>`
* `<NAME>`



## `tisvlay managed`

Toggle whether the daemon drives this TV

**Usage:** `tisvlay managed <ID> <MANAGED>`

###### **Arguments:**

* `<ID>`
* `<MANAGED>`

  Possible values: `true`, `false`




## `tisvlay config`

Read a TV's config (vendor secrets redacted)

**Usage:** `tisvlay config <ID>`

###### **Arguments:**

* `<ID>`



## `tisvlay config-set`

Patch a TV's config with a JSON object (null unsets a vendor key)

**Usage:** `tisvlay config-set <ID> <PATCH>`

###### **Arguments:**

* `<ID>`
* `<PATCH>`



## `tisvlay repair`

Re-run the pairing handshake for an existing TV

**Usage:** `tisvlay repair <ID>`

###### **Arguments:**

* `<ID>`



## `tisvlay power`

Power a TV on or off

**Usage:** `tisvlay power <ID> <STATE>`

###### **Arguments:**

* `<ID>`
* `<STATE>`

  Possible values: `on`, `off`




## `tisvlay reconnect`

Ask a downed TV to reconnect

**Usage:** `tisvlay reconnect <ID>`

###### **Arguments:**

* `<ID>`



## `tisvlay remove`

Remove a TV (deletes all its keys)

**Usage:** `tisvlay remove <ID>`

###### **Arguments:**

* `<ID>`



## `tisvlay mute`

Mute or unmute a TV

**Usage:** `tisvlay mute <ID> <MUTED>`

###### **Arguments:**

* `<ID>`
* `<MUTED>`

  Possible values: `true`, `false`




## `tisvlay primary-input`

Set (or with no value clear) the TV's primary input

**Usage:** `tisvlay primary-input <ID> [INPUT]`

###### **Arguments:**

* `<ID>`
* `<INPUT>`



## `tisvlay assign`

Assign (or with no value clear) the TV's system profile

**Usage:** `tisvlay assign <ID> [PROFILE]`

###### **Arguments:**

* `<ID>`
* `<PROFILE>`



## `tisvlay vendors`

List registered vendor backends with their capabilities

**Usage:** `tisvlay vendors`



## `tisvlay schema`

Show a vendor's pairing form schema

**Usage:** `tisvlay schema <VENDOR>`

###### **Arguments:**

* `<VENDOR>`



## `tisvlay discover-session`

Open-ended discovery session: findings stream until ctrl-c

**Usage:** `tisvlay discover-session [OPTIONS]`

###### **Options:**

* `--vendor <VENDOR>`



## `tisvlay profile`

System-profile catalog (daemon-side)

**Usage:** `tisvlay profile <COMMAND>`

###### **Subcommands:**

* `list` — List profile bodies from the catalog caches
* `create` — Create a profile: `--body` is a SystemProfileConfig body
* `update` — Replace a profile's body (full JSON body; name required inside)
* `duplicate` — Copy a profile under a new name
* `delete` — 
* `defaults` — The stock mapping unset fields inherit
* `effective` — One TV's resolved mapping (assigned profile over stock)



## `tisvlay profile list`

List profile bodies from the catalog caches

**Usage:** `tisvlay profile list`



## `tisvlay profile create`

Create a profile: `--body` is a SystemProfileConfig body

**Usage:** `tisvlay profile create [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--body <BODY>`



## `tisvlay profile update`

Replace a profile's body (full JSON body; name required inside)

**Usage:** `tisvlay profile update <ID> <BODY>`

###### **Arguments:**

* `<ID>`
* `<BODY>`



## `tisvlay profile duplicate`

Copy a profile under a new name

**Usage:** `tisvlay profile duplicate <ID> <NAME>`

###### **Arguments:**

* `<ID>`
* `<NAME>`



## `tisvlay profile delete`

**Usage:** `tisvlay profile delete <ID>`

###### **Arguments:**

* `<ID>`



## `tisvlay profile defaults`

The stock mapping unset fields inherit

**Usage:** `tisvlay profile defaults`



## `tisvlay profile effective`

One TV's resolved mapping (assigned profile over stock)

**Usage:** `tisvlay profile effective <TV>`

###### **Arguments:**

* `<TV>`



## `tisvlay user-profile`

User-profile catalog (session-service side, via --user-socket)

**Usage:** `tisvlay user-profile <COMMAND>`

###### **Subcommands:**

* `list` — 
* `create` — Create a profile: `--body` is a UserProfileConfig body
* `update` — 
* `duplicate` — 
* `delete` — 
* `defaults` — 
* `effective` — 
* `assign` — Assign (or with no value clear) a TV's user profile



## `tisvlay user-profile list`

**Usage:** `tisvlay user-profile list`



## `tisvlay user-profile create`

Create a profile: `--body` is a UserProfileConfig body

**Usage:** `tisvlay user-profile create [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--body <BODY>`



## `tisvlay user-profile update`

**Usage:** `tisvlay user-profile update <ID> <BODY>`

###### **Arguments:**

* `<ID>`
* `<BODY>`



## `tisvlay user-profile duplicate`

**Usage:** `tisvlay user-profile duplicate <ID> <NAME>`

###### **Arguments:**

* `<ID>`
* `<NAME>`



## `tisvlay user-profile delete`

**Usage:** `tisvlay user-profile delete <ID>`

###### **Arguments:**

* `<ID>`



## `tisvlay user-profile defaults`

**Usage:** `tisvlay user-profile defaults`



## `tisvlay user-profile effective`

**Usage:** `tisvlay user-profile effective <TV>`

###### **Arguments:**

* `<TV>`



## `tisvlay user-profile assign`

Assign (or with no value clear) a TV's user profile

**Usage:** `tisvlay user-profile assign <TV> [PROFILE]`

###### **Arguments:**

* `<TV>`
* `<PROFILE>`



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
