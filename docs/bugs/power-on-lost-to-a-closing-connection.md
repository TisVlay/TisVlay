# A power-on sent to a closing connection is lost behind an OK reply

**Status:** closed (2026-11-01). A power-on whose link closes under it now reaches the world as `BackendEvent::PowerOnLost`, which re-arms it as a wake dial; `a_power_on_lost_to_a_closing_link_rearms_as_a_wake` in the world suite pins it. Opened 2026-10-24. Found while verifying the CEC device-presence work ([docs/watchers/devices.md](../watchers/devices.md)).

**Severity:** low. The window is short, but nothing reports the loss, and an automated profile action can hit it as easily as a person.

## Symptom

`tisvlay power <tv> on` returns success, nothing happens, and the TV stays off. The only trace is a `backend command failed` line in the daemon's journal with `cmd=PowerOn error="Connection closed"`.

Reproduced on a CEC TV: stop the adapter's `pulse8-cec-inputattach` unit and send the power-on at once. The world still held the connection, so the power-on went in-band and failed with the adapter already gone; a moment later the device watcher reported the adapter unavailable.

## Cause

A connected TV takes its power-on in-band: `power_on_tv` hands `BackendCmd::PowerOn` to the connection task and the rpc answers once the command is sent, not when the TV completes it. When the connection is closing at that moment (the adapter vanished, or a network TV's socket is dying), the task fails the command, and the failure only reaches the journal: `BackendCmd::PowerOn` carries no reply (the rpc was already answered), and nothing re-routes the intent.

The window is the time between the transport going away and the world dropping the handle: for CEC the connection's monitor polls every 250 ms, for LG the socket's close.

## Fix

When an in-band power-on fails because the connection closed, re-arm it as a wake dial (`redial(e, true)`), so it takes the path a power-on to a disconnected TV takes: held while the route or device is missing, sent when it returns within the hold limit, cancelled by a later off, and reported on `progress/power` either way. The world had no way to learn the failure: `CommandDone { reply, result }` covers only commands that carry a reply, and a power-on doesn't. So the power-on gets its own event: the connection task sends `PowerOnLost` when the command fails with `ConnectionClosed` or leaves the link down, ahead of its `Disconnected`, and the world redials with wake.

Only a standby-reachable transport (CEC) takes an in-band power-on, since a network TV's power-on routes by transport instead, so that's the backend this protects.

On hardware (a Pulse-Eight adapter whose inputattach unit is stopped under a power-on, 2026-11-01) the re-arm can race the device watcher. The re-armed dial starts in the milliseconds before the watcher reports the adapter gone, fails to open the device, and counts as one attempt of its budget; when the report lands first, the failure is returned instead (`dial failed with its dependency absent`). Either way the wake is then held and sent when the adapter returns. Worth changing only if a budget that tight ever matters.
