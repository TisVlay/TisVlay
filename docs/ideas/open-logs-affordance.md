# "Open logs" affordance

User asked about a journal-viewer launcher; researched in Plan 5 and dropped because there's no portable `journal://` URI scheme. Options that surfaced:

- **(a)** Spawn `ksystemlog` directly via `xdg-open` — best UX when KSystemLog is installed, degrades on GNOME / minimal installs.
- **(b)** Spawn a terminal with `journalctl -u tisvlayd -f` pre-typed — most accurate but UX is fragile (`xdg-terminal-exec` is freedesktop-spec but spotty).
- **(c)** Copy-to-clipboard a `journalctl` command with a toast saying "Open a terminal and paste" — simplest and degradation-friendly.
- **(d)** Embed a small log viewer inside the GUI (read systemd journal via `systemd::journal::Journal::open` or shell out + parse) — most polished but a sizable engineering chunk.

Leading recommendation: **(a) with (c) as fallback** — try `xdg-open` on the KSystemLog desktop file first; on failure, fall back to copy-to-clipboard with the journalctl command. Defer (d) until a real debugging-support story justifies the engineering cost.

Surface placement: both — About pane gets the always-on shortcut; inline-error messages with `kind == "internal"` get a contextual "View logs" action button.
