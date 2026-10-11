# Shared supervised-task helper

## Problem

Five loops hand-roll the same "keep this task alive" shape with different spellings:

- the tray's settings service: supervised client link into the session router (fail-fast open + retry loop, per the serve-side law),
- the session service's wayland idle adapter: re-register on watch death, retry every 5 s when the compositor is briefly gone,
- the daemon's netlink monitor: reopen the kernel connection with a capped backoff, resyncing its kept view on every reconnect ([watchers/network.md](../watchers/network.md) § Staying connected);
- the daemon's seat-access watch: a fresh system-bus connection every 30 s after a loss, warning once until a watch runs long enough to count as working;
- the daemon's logind watcher: the same shape on a 5 s delay, since a suspend in the gap leaves the TVs on. It ran unsupervised until a QA run showed dbus-broker evicting every peer of a user over its quota: the watcher's streams ended and sleep handling stopped silently for the daemon's lifetime.

Each grew its loop independently, so restart policy, backoff, and logging differ by accident rather than by design.

## Idea

A small `tisvlay-runtime` helper (binary glue, not core's published API) that makes "supervised task" a named concept:

```rust
pub async fn supervised<F, Fut>(name: &str, backoff: Duration, mut factory: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    loop {
        match factory().await {
            Ok(()) => break, // clean shutdown (channel closed)
            Err(e) => warn!(task = name, error = %e, "supervised task failed; restarting"),
        }
        tokio::time::sleep(backoff).await;
    }
}
```

plus whatever per-caller nuance survives contact (the idle adapter also re-registers on parameter *change*, not just failure; the tray link distinguishes hub-restart from fatal config errors).

## Why deferred

Semantic, cross-crate change touching five supervision policies that are individually field-validated; the wrong unification (e.g. restarting the netlink monitor into a half-initialized state) would be worse than the duplication. Do it deliberately, with the per-service shutdown semantics written down first.

## Related: the daemon's core-task supervisor

The daemon's core tasks (world reactor, zenoh bridge, store writer) already have a supervisor, `Stack::run_until` (see [architecture.md](../architecture.md) § The world). Its policy is exit-on-death, not restart-in-process: a dead core task ends the process in order and systemd restarts it from the store, because those tasks share state that can't be rebuilt piecemeal. This idea is about the other shape, the per-service loops that restart in place: the tray link, the idle adapter, the netlink monitor, and the seat-access and logind watches.
