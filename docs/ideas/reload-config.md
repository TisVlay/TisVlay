# Reload config from disk (daemon, session, tray)

The GUI's sidebar "Reload config" button calls `reload_config()` on the daemon, session and tray clients, and all three are no-ops on the zenoh stack: the button does nothing.

## Why it stopped working

On the served-D-Bus stack (≤ v0.2.0), `config.toml` was the daemon's source of truth. `Manager1.ReloadConfig` re-read it, diffed it against the in-memory catalog, and emitted the same `Added`/`Changed`/`Removed` signals an external client's edit would ([design.md](../design.md), "ReloadConfig emits the same per-UUID signals"). `Session1.ReloadConfig` and the tray's did the same for their files.

The zenoh switchover (`dfda1baa`) moved the source of truth into the ECS world. `daemon.toml` became a debounced snapshot the daemon writes and replays at boot ([architecture.md § Persistence](../architecture.md#persistence)), and `session.toml`/`tray.toml` work the same way. No reload rpc was ported; the SDK kept `reload_config()` as a no-op so the GUI could move stacks with import swaps.

## Path forward

- **Rpcs:** `manager/rpc/reload`, `user/profile/rpc/reload` and `user/tray/rpc/reload`, each declared in its endpoint family.
- **Daemon:** re-read the store, diff TVs and system profiles by id against the world, and apply the diff as ordinary commands (`AddTv`, remove, `UpdateConfig`, profile upserts and deletes). The published keys then update every client's mirror, which is the zenoh equivalent of the old signals; a reload stays observationally identical to a client edit.
- **Session and tray:** re-read their files, apply the user-profile diff or the merged settings, and republish.
- **The overwrite race:** the daemon rewrites its store on any config-shaped change (a pair-time MAC fill, a rename), so a hand edit made while it runs can be overwritten before Reload is pressed. Track what the last write produced (mtime plus a content hash) and refuse, or warn, when the file on disk is neither that nor a deliberate edit since; or have the stores write only on shutdown and explicit saves.
- **GUI:** wire the button to the three rpcs and surface a refusal in the existing banner slots.

## Trigger conditions

- Next up after the API-hygiene PR (the owner's call): the button is visible and dead today.
