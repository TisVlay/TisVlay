# Known Bugs & Issues

## Open

- [Upstream: linux-cec re-claim drops the OSD name](upstream-linux-cec-reclaim-drops-osd-name.md) — draft for linux-cec, not filed; held until our workaround (`8f764d5`) soaks
- [Upstream: linux-cec can't set the log-address flags](upstream-linux-cec-log-addr-flags.md) — draft merge request for linux-cec (our fork's `rc-passthru` setter), not filed; held until the TV-controls-the-PC soak
- [Upstream: linux-cec fails to read a claim in progress](upstream-linux-cec-unclaimed-slots.md) — draft merge request for linux-cec (our fork's `rc-passthru` fix, its own MR), not filed; files with the setter's
- [Upstream: KDE's tray collapses a nested submenu on its first expansion](upstream-kde-tray-submenu-collapse.md) — open upstream (KDE 506439, MR !5763); still reproducible on Plasma 6.8.2
- [A pairing session has no daemon-side deadline](pair-tv-no-timeout.md) — LG bounds its own unaccepted prompt; other backends have no bound
- [Pulse-Eight autonomous mode pins the TV's input label](cec-pulse8-autonomous-label.md) — our `pulse8-cec` build's disarm on disconnect covers a detach and a warm reboot; a cold power-up still pins it until an HDMI replug; the only host-side prevention, untested, is turning the EEPROM's autonomous mode off, which loses the TV's wake

## Closed

- [A network TV that stays on through an outage is never reconnected](network-tv-not-redialed-after-outage.md) — a drop the TV didn't announce is redialed until it answers, nothing is held for it meanwhile, and the session re-blanks a TV that comes back lit
- [A power-on sent to a closing connection is lost behind an OK reply](power-on-lost-to-a-closing-connection.md) — re-armed as a wake dial through `BackendEvent::PowerOnLost`
- [GUI and tray show a remote-off LG TV as powered on](power-switch-active-standby.md) — all three clients key on the shared `PowerState::is_awake`
