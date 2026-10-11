```toml
# Action on system startup/boot.
on_startup = { action = "power_on", restore_input = true }

# Action when the system powers off or halts.
on_shutdown = { action = "power_off", while_remote = true }

# Action when the system reboots (including kexec).
on_reboot = { action = "none", while_remote = true }

# Action on system sleep/suspend.
on_sleep = { action = "power_off", while_remote = true }

# Action on system wake/resume.
on_wake = { action = "power_on", restore_input = true }

# Action when the matched host display connector blanks (DPMS off).
on_dpms_off = { action = "screen_off", while_remote = true }

# Action when the matched host display connector wakes (DPMS on).
on_dpms_on = "screen_on"

# Greeter idle timeout in seconds (0 to disable). Drives the
# `tisvlay-greeter` idle client at the login screen.
idle_timeout_seconds = 0

# Action when the greeter goes idle (after `idle_timeout_seconds`).
on_idle_timeout = { action = "screen_off", while_remote = true }

# Action when greeter activity resumes after idle.
on_idle_resume = "screen_on"

# When to apply automation relative to the TV's current input.
automate_when = "always"
```
