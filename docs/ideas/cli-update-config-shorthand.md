# `key=value` shorthand for `tisvlay config`

Mirrors NetworkManager's `nmcli connection modify <name|uuid> field value`: a `key=value` form for `config`, next to its JSON merge patch.

```
tisvlay config <tv> name=Den mac=AA:BB:CC:DD:EE:FF
```

It would build the same patch `tisvlay config <tv> '<json>'` sends, so a multi-field edit still lands atomically through the daemon path the GUI uses. Values need a typing rule (`managed=off` is a boolean, `mac=…` a string), which is most of the design. Accepting a TV's name in place of its UUID is its own idea: [TV names as CLI selectors](cli-tv-name-selectors.md).

Cost is mostly clap plumbing; the daemon-side surface is already there. Land when there's a concrete user request or when scripted edits start showing up in operator workflows.
