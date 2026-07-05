# Shared supervised-task helper

## Problem

Three services hand-roll the same "keep this task alive" shape with different spellings:

- the tray's settings service: supervised client link into the session router (fail-fast open + retry loop, per the serve-side law),
- the session service's wayland idle adapter: re-register on watch death, retry every 5 s when the compositor is briefly gone,
- the daemon's netlink monitor: spawn + log-on-exit (no restart at all — an rtnetlink connection failure silently ends carrier/route monitoring for the process lifetime).

Each grew its loop independently during the parity port, so restart policy, backoff, and logging differ by accident rather than by design.

## Idea

A small `tisvlay-core` helper that makes "supervised task" a named concept:

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

Semantic, cross-crate change touching three supervision policies that are individually field-validated; the wrong unification (e.g. restarting the netlink monitor into a half-initialized state) would be worse than the duplication. Do it deliberately, with the per-service shutdown semantics written down first.
