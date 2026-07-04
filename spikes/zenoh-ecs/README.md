# zenoh-ecs spike

Exploration workspace for the Zenoh + bevy_ecs rewrite. Design rationale, key-space reference, milestones, and keep/kill criteria live in [docs/plans/spike-zenoh-ecs.md](../../docs/plans/spike-zenoh-ecs.md).

This is an **independent cargo workspace** — not a member of the root workspace. Build and test from this directory, or from the repo root with `--manifest-path spikes/zenoh-ecs/Cargo.toml`.

## Quickstart

```bash
cargo run -p tvnext-daemon                 # peer session + liveliness token on $XDG_RUNTIME_DIR/tisvlay/next.sock
cargo run -p tvnext-cli -- status          # probe the daemon's liveliness token
cargo test                                 # includes the client <-> daemon liveliness integration test
```

Crate map: `tvnext-wire` (key-space vocabulary + session bootstrap), `tvnext-daemon` (world + wire host — M0: session + liveliness only), `tvnext-cli` (zenoh client).
