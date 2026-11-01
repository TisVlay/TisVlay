# A longer lease on the Unix-socket links

A sleep can drop every user-side link. systemd freezes `user.slice` around it, and zenoh's 10 s link lease keeps counting through the kernel's device suspend and resume (over 12 s where the NVIDIA driver saves video memory), so the links expire as the user processes thaw, and each service's liveliness token goes and comes back under the same zenoh session ([architecture.md](../architecture.md), empirical zenoh laws).

Since 0.5.0 that drop is harmless: every mirror follows its service per token lifetime and re-hydrates when the token returns, in a second or two ([architecture.md § Clients](../architecture.md#clients); measured on the bench on 2026-10-31, after a 62 s sleep). What's left is the drop itself: the brief "offline" and "back online" notifications, a GUI and tray that blink to "Loading", and the daemon disarming and re-arming streaming TVs.

## The idea

Lengthen the lease (and its keep-alive) on the Unix-socket links, past the longest device suspend and resume, so a sleep doesn't drop them at all. The links are local sockets: a peer that's really gone closes its socket, which zenoh sees at once, so a long lease only delays noticing a peer that's frozen or wedged, not one that exited.

## Trade-offs

- A wedged service (alive but not reading) is noticed only after the lease, so clients show it as up for longer.
- The lease is negotiated per link, so both ends' configs (`tisvlay-protocol`'s bootstrap builders) would carry it.
- It doesn't remove the need for the lifetime model: a restart, a crash and a lease that still expires on a long enough resume all drop the token anyway.

## Trigger

The notifications or the blink after each wake prove annoying in use, or a desktop's resume routinely outlasts the lease by enough to show.
