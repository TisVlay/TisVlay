# Network link recovery

A network TV whose link drops while the TV stays on is never redialed today, and a screen request sent to it while it is down is acked as already done. After a Wi-Fi router's firmware reboot on 2026-11-15 this left the office LG running its own screen saver over the PC's input for hours: TisVlay could neither see nor blank it. This plan makes the daemon redial such a TV until the network is back and restore the screen state last asked of it, and makes the next incident readable from the logs.

## What happened

Reconstructed on the packaged 0.6.0 stack. The system journal before about 08:00 was gone (a crash-looping unit outside TisVlay filled journald's size cap), so the daemon's side comes from the timestamps of its zenoh samples, and the TV's from read-only SSAP probes.

| When (CST) | What |
|---|---|
| 11-14 11:07 | The LG's last power report: on, screen off. Later screen-offs (an idle timeout, Moonshine stream starts and ends) changed nothing, so nothing was republished. |
| 11-15 01:02:39 | During the router reboot the LG's link went down. No dial followed: no `connect-failed`, never `reconnecting`. |
| 01:06:54 | The network was back (a Moonshine client reconnected). |
| 03:14:20 | A stream ended; the session sent Screen Off. The daemon acked it as satisfied without sending anything. |
| 07:56 | The TV reported power On over CEC. |
| 12:46 | Probe: the TV's power state is `Screen Saver`, HDMI 1 in front with a signal. Re-probed after five live stream events at 12:48–12:49: unchanged, with five Screen Offs acked and dropped. |

The host's own network never changed (no carrier transition, the route to the TV stayed in the FIB), so neither the carrier nor the route watcher had anything to report, and those are the only things that redial a parked network TV.

## The two code paths

- **A network TV's drop never redials.** `BackendEvent::Disconnected` (`tisvlay-world/src/systems/facts.rs`) redials only local-device TVs. `disconnect_stays_down_until_reconnect_command` pins it, reasoning that a drop means the user turned the TV off.
- **Screen Off on a disconnected TV is a no-op.** The screen dispatch (`tisvlay-world/src/systems/dispatch.rs`, `None if !on => Ok(())`) treats a disconnected TV as dark. `disconnected_screen_off_is_a_satisfied_noop` pins it, for a TV that never connected.

Both are right for a TV that went to standby and wrong for one that is on behind a network outage. The difference is visible: an LG announces a standby (its power report goes to `ActiveStandby` or `Standby`) before it closes the link, and a user power-off goes through the daemon itself.

## Decisions

1. **A drop is unannounced when the TV's last power report was awake and no power-off was on its way.** Awake is `PowerState::is_awake` (on, screen off, screen saver). On the way off: a user power-off the daemon sent, or the backend's `SendingOff`. An announced drop stays down as today. *Why:* the redial must not fight a TV the user turned off, and the last report tells the two apart without guessing from the error.
2. **An unannounced drop redials until the network is back.** The dial streak keeps retrying past the three-attempt budget on the existing backoff (3 s, doubling to a 60 s cap), the way a local device that can't be opened already does; `event/connect-failed` reports the budgeted attempts only. A user power-off, a reconnect that reports standby, or the TV's removal ends it. The dial gate is unchanged, so a parked TV (carrier or route gone) still waits for its dependency. *Why:* a router reboot takes minutes; a budget of three dials 3 s apart would not have caught last night. A plain dial never powers a TV on.
3. **The daemon keeps the last screen request per TV and re-sends it on the reconnect after an unannounced drop**, when the TV's connect snapshot differs. A screen request that arrives while such a TV is down updates it and acks; it lands on the reconnect. *Why:* the TV drifted from screen-off into its own screen saver while nobody could see it, and a redial alone would leave it there.
4. **Only after an unseen drop.** While the link is up, a panel change the TV reports (a person woke it with the TV's remote) is the new state, not a drift to undo. A screen-off to a TV the daemon has never seen awake stays a satisfied no-op. *Why:* a re-send on every drift would override a person using the TV.
5. **Visibility:** the daemon logs at info a network TV's drop with its last power state and whether it redials, the redial's success, and a screen request held for the reconnect; the session logs each profile action's outcome (acked, or the error). No wire change: the rpc's ack stays the same for held and applied requests, so "held" is the daemon's log line, not the session's. *Why:* last night's cause survived only in zenoh sample timestamps.
6. **Tests first, ignored.** The failing tests land with the plan, each `#[ignore = "…"]` with what it waits for, so every commit passes `just check`; the commit that makes one pass removes its ignore.
7. **The commits are `fix(world)` / `fix(session)`.** No key, payload, rpc or decode rule changes.

## Open for review

These were left undecided on purpose; the plan review may settle them, and what isn't taken in goes to `docs/ideas/` with an index entry.

- **Stale power state while down.** `state/power` keeps the last report (here "on, screen off") while the link is down, so clients show a state nobody can see. Marking it unknown on a drop is a value change on an existing key; the GUI, tray and CLI would need checking.
- **A tray warning for a long outage** of a managed TV's link.
- **Whether the last screen request survives a daemon restart.** In memory only would forget it on a restart mid-outage; persisting it adds a store field and a reload rule.

## Steps

Each step is a commit on `fix/network-link-recovery`, passing `just check`.

1. **The plan and the failing tests.** The mock backend learns to cut a live link without a power change (`MockHandle::cut_link`) and to refuse dials during an outage with `NetworkUnreachable` (`MockHandle::set_unreachable`); its subscription sender becomes weak, so the shared registry holds the only strong one. `tisvlay-world`'s new `link_recovery` suite: four ignored tests (a drop while awake redials until the network is back; a power-off ends the redial; a screen-off while down lands on the reconnect; the reconnect restores the requested screen-off after the TV drifted to its screen saver) and two guards that pass today (a drop after a standby report stays down; a screen woken while connected is left alone). *Verified by:* `--run-ignored only` fails the four, each at its first missing behavior (no second dial, or no reconnect); the rest of the suite passes.
2. **Redial after an unannounced drop** (decisions 1, 2). *Verified by:* the redial and power-off tests pass with their ignores gone; `disconnect_stays_down_until_reconnect_command` and the standby guard still pass; the device and carrier suites unchanged.
3. **The screen request across the drop** (decisions 3, 4). *Verified by:* the two screen tests pass with their ignores gone; `disconnected_screen_off_is_a_satisfied_noop` and the remote-wake guard still pass; the dispatch suite unchanged.
4. **Visibility** (decision 5). *Verified by:* the log lines read from a run of the hardware check below; the session's outcome logging by a unit test where the session crate's action path allows one.
5. **On the TV.** An unattended script (the test TV is the host's only display) adds an nftables rule rejecting traffic to the TV with ICMP net-unreachable, the error last night produced, holds it a few minutes, removes it, and records the daemon's timestamped samples, its journal and a read-only SSAP probe of the TV throughout. The maintainer runs it with sudo, once on 0.6.0 to reproduce and once on this branch. The script stays out of the tree. *Verified by:* on 0.6.0 the link stays down; on the branch it comes back within a backoff period of the rule's removal and the TV ends in screen-off. The same run answers whether the LG leaves Screen Off by itself when its client drops, which goes to `docs/lg/firmware-quirks.md` either way.
6. **Docs, and retire the plan.** The link model (announced and unannounced drops, the standing screen request) into `docs/architecture.md` § The world; the LG findings into `docs/lg/firmware-quirks.md`; what the review leaves of "Open for review" into `docs/ideas/`; this file deleted and its index entry removed.

## Deviations

None yet.
