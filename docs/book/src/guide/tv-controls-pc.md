# TV Controls PC

[Automation](./automation.md) runs from your PC to the TV. These settings run the other way: the TV turning off can lock your session or put the PC to sleep, and with an HDMI-CEC adapter the TV's remote and the TV turning on can act on your PC too. Out of the box none of them acts, except that TisVlay keeps the TV from waking a sleeping PC ([below](#the-tv-waking-your-pc)).

## What works where

| Setting | What it reacts to | HDMI-CEC | LG over the network |
|---|---|:---:|:---:|
| **On TV power off** | the TV turned off by its remote, its menu or its own timer | yes | yes, while TisVlay is connected to it |
| **On remote power key** | the TV's "power off this device" for your PC (on LG, in the SIMPLINK menu) | yes | no |
| **Remote keys reach the desktop** | the remote's other keys | with an adapter that supports it | no |
| **On TV power on** | the TV turning on while your PC sleeps | with a USB adapter that can wake the PC | no |

An LG that TisVlay isn't connected to, such as one turned back on with its own remote after TisVlay powered it off, isn't watched, so its next power-off goes unseen.

Each TV's page in the app has a **FROM THE TV** section showing which of these that TV supports. A setting a TV doesn't support does nothing for it.

## Setting them up

Open the **Profiles** page; the settings are in each profile's **FROM THE TV** section. Where a setting lives depends on what it acts on:

- **The machine**, in the system profile: putting the PC to sleep (**On TV power off** and **On remote power key**: `suspend` or `sleep`), waking it (**On TV power on**), and the remote's keys. These work with nobody logged in.
- **Your session**, in your user profile: locking it (**On TV power off** and **On remote power key**: `lock`). Each user sets their own.

`suspend` always suspends to RAM. `sleep` uses your machine's configured sleep, which may hibernate, and needs systemd 256 or newer. Setting both a lock and a sleep does both. The stock values are in [Configuration Files](./configuration.md#profiles).

## When they act

- **Only for a [managed](./automation.md#which-tvs-react) TV.** Turning a TV's Managed switch off stops it acting on your PC as well as TisVlay acting on it.
- **Never for TisVlay's own power-offs.** When your PC sleeps and powers the TV off, that doesn't count as the TV turning off.
- **Never while a stream has the TV [armed](./streaming.md#what-it-does-when-armed)**, so someone pressing the remote in the room doesn't sleep the PC under a streaming user.
- **The TV turning off follows Automate when.** With `on_primary`, a TV turned off while showing another input leaves your PC alone. The remote's power key doesn't: it's aimed at your PC.
- **A wake press doesn't count.** A TV switching to your PC may send it a power-key press to wake it; TisVlay ignores a press unless the TV is already showing your PC.
- **Each TV acts alone.** With two TVs, either one turning off acts, even while the other still shows your PC.
- **Nothing unlocks.** Turning the TV on never unlocks your session.

## The TV waking your PC

This needs a USB CEC adapter that can wake the PC (a Pulse-Eight can; the TV's page shows **Wakes this PC**), and a PC that wakes from USB devices, which is a firmware setting on some machines.

- **`wake`:** turning the TV on to your PC's input wakes the PC, as does switching the TV to your PC's input, or SIMPLINK's power key for it. Turning the TV on to another input doesn't.
- **`none`:** TisVlay keeps the PC asleep. A Pulse-Eight would wake it regardless, so TisVlay disconnects the adapter for the sleep and reconnects it after, and the TV shows no device for your PC in the meantime.

A Pulse-Eight wakes the PC only with its own *autonomous mode* on, as it ships. If `wake` does nothing, check it with libcec 8 or newer: stop the adapter's `inputattach` service, run `cec-client` on the adapter's serial port, and enter `am` to read the setting or `am 1` to turn it on. To keep the TV from ever waking the PC, whatever the profile says, `am 0` turns it off.

An unmanaged TV's adapter is left exactly as it is.

## The remote's keys

With **Remote keys reach the desktop** on, the TV remote's arrows, OK, back and media keys arrive as keyboard keys, and your desktop decides what they do. The remote's power keys never reach the desktop, so the remote can't put the PC to sleep that way; **On remote power key** is the setting for that. The packages install what makes that so; a source build needs one extra step ([Installation](../getting-started/installation.md#hdmi-cec-backend-optional)).
