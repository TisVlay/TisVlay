```toml
# Action on system startup/boot.
on_startup = { action = "power_on", restore_input = true }

# Action when the system powers off or halts.
on_shutdown = { action = "power_off", while_streaming = true }

# Action when the system reboots (including kexec).
on_reboot = { action = "none", while_streaming = true }

# Action on system sleep/suspend.
on_sleep = { action = "power_off", while_streaming = true }

# Action on system wake/resume.
on_wake = { action = "power_on", restore_input = true }

# Action when the matched host display connector blanks (DPMS off).
on_dpms_off = { action = "screen_off", while_streaming = true }

# Action when the matched host display connector wakes (DPMS on).
on_dpms_on = "screen_on"

# Greeter idle timeout in seconds (0 to disable). Drives the
# `tisvlay-greeter` idle client at the login screen.
idle_timeout_seconds = 0

# Action when the greeter goes idle (after `idle_timeout_seconds`).
on_idle_timeout = { action = "screen_off", while_streaming = true }

# Action when greeter activity resumes after idle.
on_idle_resume = "screen_on"

# When to apply automation relative to the TV's current input.
automate_when = "always"

# What the machine does when the TV powers off by itself or from its
# own remote: `suspend` suspends to RAM, `sleep` uses the host's
# configured sleep (which may hibernate). TisVlay's own power-offs
# never count, and nothing happens while a stream arms the TV.
# Default `none`.
on_tv_power_off = "none"

# What the machine does when the TV's remote powers this PC off (on
# HDMI-CEC, the TV's "power off this device"): `suspend` suspends to
# RAM, `sleep` uses the host's configured sleep. It acts whatever the
# automation settings, since the press is explicit, but nothing
# happens while a stream arms the TV. Default `none`.
on_remote_power = "none"

# Whether the TV remote's keys reach this PC's desktop (on HDMI-CEC,
# as the kernel's remote-control input device). What each key does is
# the desktop's to decide. The packages' keymap keeps the remote's
# power keys away from the desktop; without it (a source build), the
# power key can put the PC to sleep. Default `false`.
remote_keys = false

# What the TV powering on does while this PC sleeps: `wake` lets it
# wake the PC (on HDMI-CEC, through a USB adapter allowed to wake the
# system, armed for every sleep), and `none` keeps the PC asleep (on
# HDMI-CEC, the adapter's USB port is disabled for the sleep, so the
# TV has no CEC device to show for this PC until it resumes). Where
# the TV comes up on another input, `wake` may still wake the PC.
# Default `none`.
on_tv_power_on = "none"
```
