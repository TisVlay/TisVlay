# Link-recovery follow-ups

What the fix for [a network TV that stays on through an outage](../bugs/network-tv-not-redialed-after-outage.md) left out. Each was weighed and deferred for a stated reason; none is needed for that incident. The rules they build on are in [architecture.md](../architecture.md) § The world.

## A link that dies silently

The LG client sends no WebSocket ping, and its reader only answers the TV's. A link whose peer vanishes without a close (a half-open socket) isn't noticed until the next command times out, so the redial doesn't start until then. The incident's drop was a clean close.

**Path:** a periodic ping from the client, with a missed pong closing the link, which then reads as an unannounced drop.

**Why deferred:** it needs an interval and a deadline, two new time constants in a fix that added none, and the interval trades detection delay against keeping a TV's network stack busy. `docs/ideas/zenoh-link-lease.md` has the same shape for the IPC side.

## An unseen TV's power state

`state/power` keeps the TV's last report while its link is down, so the GUI, tray and CLI can show "on, screen off" for a TV nobody can see. `state/link` says it is down, and the session no longer trusts the panel bit of a disconnected TV, so nothing acts on the stale value.

**Path:** publish the existing `unknown` power value when a drop is unannounced, or have each client grey the last-known state from `state/link`. The first is a value change on an existing key and needs every client checked for how it renders `unknown`; a new value such as "unreachable" would be wire-visible.

**Why deferred:** cosmetic once the session's rule landed, and the world itself reads the last report to tell an announced drop from an outage.

## The daemon's own reasons for a dark panel

When a link returns, the session decides again about the panel from what it knows: whether the desk is still idle. The world owns reasons of its own that nothing re-decides: a connector's DPMS-off, and the greeter's idle with no session logged in. A profile action of theirs refused during an outage is logged and not made good, so a TV back lit over a dark desk stays lit until the next event.

**Path:** the world keeps those conditions as levels it can read (a connector's DPMS state, the greeter's idle flag) and re-runs their actions on a managed TV's reconnect when they still hold. `docs/ideas/eager-remote-state-refresh.md` touches the same DPMS path.

**Why deferred:** each condition needs to be made a level first, and the incident ran with a session.

## A stream's dark panel

The session watches for drift only after an idle action. A TV blanked by a stream's start that comes back lit from an outage while the stream runs isn't re-blanked, and a stream action refused during the outage isn't sent again.

**Path:** a drift window for streams, opening at the stream's start action and closing at its end or at any event that acts on the TV (an unlock that lights it must not be undone).

**Why deferred:** a stream has no timer of its own to serve as the grace an idle timeout gives, so the window needs a rule the reviews couldn't settle. Idle usually covers it: a desk left streaming is idle too.

## A wake owed on the link's return

An unlock, a wake at the lock screen or an idle resume during an outage can't light the TV, and their wake dial falls back to the plain redial once its budget is spent. Nothing lights the TV when the link returns, so a person back at the desk has a dark TV until they touch something again.

**Path:** the mirror of the session's dark rule: a desk that is active with a TV found dark on its return re-sends the resume action.

**Why deferred:** "the desk is active" doesn't mean "the panel is wanted lit": a manual screen-off holds across activity today, and a mirror rule would undo it.

## An address that moves during the outage

A redial dials the stored address. A router reboot is when DHCP leases move, and a TV that comes back at a new address is dialed at the old one for good, logged at debug past the third attempt, until it is re-paired or its address edited.

**Path:** discovery during a long redial (the TV's SSDP identity is stable), updating the stored address when the TV is found elsewhere; [ssdp-notify-passive-listener.md](ssdp-notify-passive-listener.md) would make it passive. A periodic info line for a redial that has run long would at least make it visible.

**Why deferred:** the bench TV has a reserved address, and rewriting a stored address from the network needs its own care (which identity is trusted).

## A laptop away from home

With no route to the TV the dial gate defers and nothing is sent. On a foreign network with a default route, the route resolves and the redial dials the TV's private address about once a minute, where it used to give up after three.

**Path:** a notion of "home": the redial runs only on the network the TV was last reached through (the gateway's address, or the interface's network identity).

**Why deferred:** harmless traffic, and telling networks apart is a feature of its own.

## A warning for a long outage

Nothing tells the user a managed TV has been unreachable for a long time. `event/connect-failed` reports only the first three attempts of a streak and has no consumer; the tray's notifications cover wake failures.

**Path:** a tray notification when a managed TV's link has been down past a threshold while the desk is in use, with the threshold as a tray setting.

**Why deferred:** a new user-facing feature with a setting, not part of a fix.
