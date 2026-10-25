```toml
# Action on session lock event.
on_session_lock = { action = "screen_off", while_streaming = true }

# Action on session unlock event.
on_session_unlock = { action = "screen_on", restore_input = true }

# Action when user interacts with the lock screen greeter (before unlocking).
on_lock_wake = "screen_on"

# Idle timeout in seconds while the screen is locked (0 to
# disable) — how long a bumped-but-not-unlocked lock screen stays
# awake before `on_idle_timeout` re-blanks it.
locked_idle_timeout_seconds = 0

# Idle timeout in seconds while unlocked (0 to disable) — the
# in-session idle threshold for `on_idle_timeout`/`on_idle_resume`.
unlocked_idle_timeout_seconds = 0

# Action on idle timeout (either state's timer; each timer only
# fires in its own lock state).
on_idle_timeout = { action = "screen_off", while_streaming = true }

# Action when activity resumes after an unlocked idle timeout.
# Activity while locked dispatches `on_lock_wake` instead.
on_idle_resume = "screen_on"

# Action when a stream starts.
on_stream_start = "screen_off"

# Action when the last stream ends.
on_stream_end = "screen_off"

# Action on the first local input after a stream ends —
# wakes the panel that the disconnect blank left dark for the
# returning person.
on_local_return = { action = "screen_on", restore_input = true }

# Whether an unattributed screen-capture signal arms the remote
# session. Attributed sources (streaming apps, hooks, logind) always
# arm; this decides only the bare screencast case (OBS, browser
# tab-share). Default off.
screencast_arms = false
```
