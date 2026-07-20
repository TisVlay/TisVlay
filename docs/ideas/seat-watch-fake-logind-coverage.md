# Seat-watch regression coverage over a fake logind Seat surface

The active-seat socket ACL (`crates/tisvlay-daemon/src/seat.rs`) has no regression coverage for its qualifying gates: `Class=user` ∧ `Type∈{x11,wayland}` ∧ `Remote=no`, keyed off each seat's `ActiveSession` property. The shipped tests cover the ACL blob layout, the kernel round-trip, and both directions of the grant-mid-run contract (chmod-based, kernel-level) — but nothing exercises `watch_seats`/`qualifying_uid` themselves, because `tisvlay-test`'s fake logind implements only the Manager/sleep/session surfaces the existing daemon and session tests need, not Seat objects.

## Why this is worth fixing

- The gates are security-relevant and default-on: a refactor that drops the `Remote=no` check (or scans sessions by `Active=yes`, the documented trap — seatless SSH sessions self-report active) would widen the grant and CI would stay green.
- The watch's structural behaviors — re-enumeration on `SeatNew`/`SeatRemoved`, the pinned resync deadline, hold-state on a failed seat read — are all testable deterministically against a scripted fake.

## Sketch

Extend `tisvlay-test`'s fake logind with `org.freedesktop.login1.Seat` objects exposing an `ActiveSession` property (settable by the test), plus fake `Session` objects with `User`/`Class`/`Type`/`Remote` properties. Tests then drive: greeter session active → no user entry; user wayland session active → entry for its uid; `Remote=yes` or `Type=tty` variants → no entry; ActiveSession flip → entry follows; seat removal mid-watch → re-enumeration. Assert via the ACL xattr on a temp socket (the same `getfacl`-equivalent read the acl unit tests use).
