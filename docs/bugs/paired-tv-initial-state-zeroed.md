## Newly paired TV has zeroed D-Bus properties until next subscription update

**Status**: open
**Severity**: medium — tray shows wrong state (volume 0, no input, backlight 0) for up to 3 minutes after pairing
**Found by**: manual testing — paired from scratch, tray showed zeroed state

### Problem

When a TV is paired via `AddTv`, the tray immediately shows the new
device but all dynamic properties (volume, input, backlight, power
state) are zeroed out or empty. The correct state doesn't appear until
the next SSAP subscription update arrives from the TV, which can take
minutes depending on when the TV next pushes a state change.

### Root cause

The pairing flow has a race between subscription and D-Bus interface
registration:

1. `TvManager::pair_tv()` connects and calls `try_subscribe_state()`.
   The LG backend starts SSAP subscriptions (power, volume, inputs,
   foreground app, backlight). The first subscription responses arrive
   and are forwarded to the `state_change_tx` channel.

2. `pair_tv()` returns the `tv_id`.

3. The `AddTv` D-Bus handler in `manager.rs` then calls
   `register_tv_interfaces()`, which creates the D-Bus interface
   structs with **hardcoded default state**:

   ```rust
   TvPowerService { power_state: "unknown".to_string(), .. }
   TvVolumeService { volume: 0, muted: false, .. }
   TvInputService { current_input: String::new(), available_inputs: Vec::new(), .. }
   TvBacklightService { backlight: 0, .. }
   ```

4. The subscription updates from step 1 were sent to the
   `state_change_forwarder` channel **before** the D-Bus interfaces
   existed. The forwarder calls `object_server.interface::<_, T>(path)`
   which returns `Err` (interface not registered yet), so the updates
   are silently dropped.

5. The tray fetches state via `GetManagedObjects` and sees the zeroed
   defaults.

6. Eventually the TV sends a new subscription update (e.g., a
   periodic backlight push) and the forwarder can now update the
   registered interfaces. Only then does the tray see correct state.

### Evidence from journald

```
14:45:35  Subscribed to backend state updates
14:45:35  Forwarding subscription update to D-Bus properties   ← arrives before interfaces registered
14:45:35  TV paired successfully                                ← pair_tv() returns
14:45:35  D-Bus: ListSystemProfiles                             ← tray polls, sees zeroed state
...
14:48:31  Forwarding subscription update to D-Bus properties   ← ~3 min later, first successful forward
```

Only one forwarding event at 14:45:35, then a 3-minute gap until
14:48:31 when the TV pushes new state. The initial update was lost.

### Affected files

- `crates/tisvlayd/src/dbus/tv.rs` — `register_tv_interfaces()` uses hardcoded defaults
- `crates/tisvlayd/src/dbus/manager.rs` — `AddTv` registers interfaces after `pair_tv()` returns
- `crates/tisvlayd/src/tv_manager.rs` — `pair_tv()` subscribes before interfaces exist

### Possible fix directions

1. **Read cached state at registration time**: `register_tv_interfaces()` reads the `ManagedTv`'s latest `TvState` from the manager instead of using hardcoded defaults. The subscription update that arrived during `pair_tv()` would have been cached on the `ManagedTv` even if the D-Bus forwarder couldn't deliver it.

2. **Register interfaces before subscribing**: Move `register_tv_interfaces()` into `pair_tv()` (or before it returns) so the forwarder can deliver updates immediately. But this couples D-Bus registration into the manager, which currently doesn't know about D-Bus.

3. **Replay latest state after registration**: After `register_tv_interfaces()` returns, have the `AddTv` handler trigger a one-shot state refresh from the manager's cached state. This avoids changing the registration order.
