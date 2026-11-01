# Tray notification settings: a partial `tray.toml`

The tray persists its notification switches to `tray.toml` as top-level keys (`tv_connection`, `tv_pairing`, `tv_wake`, `daemon_offline`, plus a `version`). `NotificationSettings` (`tisvlay-protocol`'s `rpc.rs`) has no per-field defaults, so a file that omits a switch doesn't parse: the tray refuses it at startup and on reload. Hand-editing the file is a supported path (the GUI's **Config** button opens it), and someone turning off one category may well write only that key.

Two ways to settle it:

- **Per-field defaults** (`#[serde(default = …)]` on the persisted form only, since the wire body is a full snapshot): a partial file means "these switches changed, the rest stay default-on".
- **Keep it strict and say so**: the book's Configuration Files page lists the four keys as required, and the reload error names the missing one.

Either fits [strict-decoding.md](strict-decoding.md)'s question of how the stores treat fields they don't expect; this is the other half, fields they do expect but don't find.

**Trigger:** a user reports a hand edit of `tray.toml` that the tray refused, or the strict-decoding work picks a policy for the stores.

The D-Bus-era items once filed here (a shared trait over the per-service clients, converging their proxy property caches) went with the served D-Bus stack.
