# A power-on sent to a closing connection is lost behind an OK reply

**Status:** open (2026-10-24). Found while verifying the CEC device-presence work ([docs/watchers/devices.md](../watchers/devices.md)).

**Severity:** low. The window is short, but nothing reports the loss, and an automated profile action can hit it as easily as a person.

## Symptom

`tisvlay power <tv> on` returns success, nothing happens, and the TV stays off. The only trace is a `backend command failed` line in the daemon's journal with `cmd=PowerOn error="Connection closed"`.

Reproduced on a CEC TV: stop the adapter's `pulse8-cec-inputattach` unit and send the power-on at once. The world still held the connection, so the power-on went in-band and failed with the adapter already gone; a moment later the device watcher reported the adapter unavailable.

## Cause

A connected TV takes its power-on in-band: `power_on_tv` hands `BackendCmd::PowerOn` to the connection task and the rpc answers once the command is sent, not when the TV completes it. When the connection is closing at that moment (the adapter vanished, or a network TV's socket is dying), the task fails the command, and the failure only reaches the journal: `BackendCmd::PowerOn` carries no reply (the rpc was already answered), and nothing re-routes the intent.

The window is the time between the transport going away and the world dropping the handle: for CEC the connection's monitor polls every 250 ms, for LG the socket's close.

## Proposed fix

When an in-band power-on fails because the connection closed, re-arm it as a wake dial (`redial(e, true)`), so it takes the path a power-on to a disconnected TV takes: held while the route or device is missing, sent when it returns within the hold limit, cancelled by a later off, and reported on `progress/power` either way. That needs the world to learn which command failed: today the failure arrives as `CommandDone { reply, result }` for commands that carry a reply, and as a log line for those that don't. Either a power-on carries its own failure event, or `CommandDone` gains the command kind.

Both backends benefit; the fix belongs in the world, not a backend.
