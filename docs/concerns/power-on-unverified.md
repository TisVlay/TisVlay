## A power-on isn't verified, so a refused wake leaves the TV dark without a word

**Priority**: low
**Found by**: the hardware checks for the connected power-on routing (#228, 2026-11-01)

### Context

Screen actions ride a reliable tier: `DispatchVerify` checks the cached power state after the backend's verify delay, retries within a budget, escalates, and reports an unmet intent on `event/dispatch-failed` ([architecture.md](../architecture.md#the-world)). A power-on has none of that. Its rpc answers once the power-on is routed (sent in place, or the wake dial armed), and a wake dial ends at its connection: `progress/power` reports `connected` whatever power state the TV reports on that connection.

Two ways that ends with a dark TV:

- **A refused wake.** LG ignores a network wake for about a second after a remote-off ([firmware quirks](../lg/firmware-quirks.md#recent-remote-off-latches-lg-firmware-against-turnon-for-1-s)), and the TV goes on into standby.
- **A dial that connects too early.** For a few seconds after a remote-off, an LG in `ActiveStandby` still accepts SSAP, so a wake dial can connect on its first attempt. The connector resends WoL only between failed attempts, so that dial sends one. In the first hardware check, a power-on 1 s after a remote-off connected on attempt 1 and the TV stayed in `activestandby` until the remote turned it on.

Either way `progress/power` says `connected`, the CLI's `power` getter reads `off`, and nothing reaches `dispatch-failed`.

### Risk

Low on the hardware measured: in the same session, a C3 took the first WoL within 0.25 s from 2 s after a CEC power-off and from 4 s after an LG one. The window is the second after a remote-off, which a person reaching for the tray rarely hits, but an automated `on_wake` or `on_startup` could.

### Mitigation options

- **Verify a power-on as the screen actions are verified.** Arm a power intent with the dial, check the cached power state after the verify delay, re-send the wake within a budget, and publish `dispatch-failed` when it's spent. It also gives clients a real terminal outcome for a power-on.
- **Resend in the connector.** In LG's `wake_and_connect`, keep sending WoL after connecting while the TV reports a wake-needy state, up to a deadline. Narrower, and LG-only.

### Re-evaluation triggers

- A report of a power-on that left the TV dark.
- A client that needs a power-on's outcome, such as the GUI's staged power-on progress.
