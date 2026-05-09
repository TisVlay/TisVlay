# CLI shorthand for `Tv1.UpdateConfig`

Was scoped as an optional follow-up after the GUI's edit-mode toggle landed. Mirrors NetworkManager's `nmcli connection modify <name|uuid> field value`:

```
tisvlay tvs <name|uuid> set <field>=<value> [<field>=<value>…]
```

Wraps `Tv1.UpdateConfig(json_patch)` so multi-field edits round-trip atomically through the same daemon path the GUI uses. Includes the long-deferred name/UUID dual-lookup for TV identifier args (today the CLI already accepts UUID but not name; this would unify the two).

Cost is mostly clap plumbing — the daemon-side surface is already there. Land when there's a concrete user request or when scripted edits start showing up in operator workflows.
