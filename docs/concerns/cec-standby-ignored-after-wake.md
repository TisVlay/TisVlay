# An LG C3 may ignore CEC `<Standby>` just after waking (unconfirmed)

**Priority:** low until confirmed. **Found:** 2026-10-24, while verifying the CEC device-presence work ([docs/watchers/devices.md](../watchers/devices.md)) on an LG C3 with a Pulse-Eight adapter.

## Observation

Three `tisvlay power <cec-tv> off` calls left the TV on (`state/power` still `active` 10 s later), and each came within seconds of the TV being turned on with its remote (the TV is the PC's display, so it was woken to read the terminal). Every power-off sent once the TV had been on for a while worked. The successful case was traced: `40 36` (`<Standby>`) ACKed, then the TV's `0f 36` broadcast and `90 01` (standby) replies. No failing case was traced, so it's open whether the TV received and ignored the message, or something woke it back up within the 10 s.

A related observation (2026-10-31): at the lock screen, a 5 s locked idle timeout turned the screen off and on twice while the password was typed, each emulated over CEC as `<Standby>` and `<Image View On>`. After that the TV answered every power query with standby and ignored `<Image View On>` while its panel showed the PC (the LG record read `active`), until it was power-cycled with its remote. Rapid off-on cycles may leave the C3's CEC side stuck, the same family as the above.

## Risk

If the C3 does ignore `<Standby>` for a while after waking, a profile action fired in that window is lost silently: an idle "screen off" (emulated as power off on CEC) just after the TV woke would leave it on. The rpc reports success either way, since the daemon acks on send.

## Confirming it

Turn the TV on with its remote, then within a few seconds:

```fish
echo 2 | sudo tee /sys/module/cec/parameters/debug
tisvlay power <cec-tv> off
sleep 10
journalctl -k --since '-15s' --no-pager | grep -E 'cec_(transmit_msg_fh|transmit_done_ts|received_msg_ts)'
tisvlay --json list | jq -c '.[] | {n: ."meta/identity".name, s: ."state/power".state}'
echo 0 | sudo tee /sys/module/cec/parameters/debug
```

- `40 36` ACKed with no `0f 36` from the TV, and the state still `active`: the TV ignores it. The mitigation would be a verify-and-retry on CEC power-off, the screen-dispatch verify loop's pattern.
- `40 36` followed by `0f 36`, then a wake message (`x0 04`, `xf 82`) from another device: something else woke it; note the initiator.
- No `40 36`: the command never reached the bus, which is a daemon bug.

## Re-evaluate when

A capture like the one above is taken, or a user reports an idle or lock action that didn't turn a CEC TV off. If confirmed as a TV quirk, record it under [docs/cec/protocol.md](../cec/protocol.md) § Quirks.
