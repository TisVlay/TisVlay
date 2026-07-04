# zenoh-ecs spike

Exploration workspace for the Zenoh + bevy_ecs rewrite. Design rationale, key-space reference, milestones, and keep/kill criteria live in [docs/plans/spike-zenoh-ecs.md](../../docs/plans/spike-zenoh-ecs.md).

This is an **independent cargo workspace** — not a member of the root workspace. Build and test from this directory, or from the repo root with `--manifest-path spikes/zenoh-ecs/Cargo.toml`.

## Quickstart

```bash
cargo run -p tvnext-daemon                        # daemon: world + bridge on $XDG_RUNTIME_DIR/tisvlay/next.sock
cargo run -p tvnext-daemon -- --config tvs.toml   # …seeding TVs from a boot config
cargo run -p tvnext-cli -- status                 # probe the daemon's liveliness token
cargo run -p tvnext-cli -- add Den --address mock://den
cargo run -p tvnext-cli -- list                   # hydrate from publisher caches (subscriber history)
cargo run -p tvnext-cli -- watch                  # stream every data key: history, then live
cargo run -p tvnext-cli -- set-volume <uuid> 40
cargo test                                        # world + wire integration suites
```

Boot config shape:

```toml
[[tv]]
name = "Den"
vendor = "mock"
config = { address = "mock://den" }
```

Crate map: `tvnext-wire` (key-space vocabulary, payload structs, rpc bodies, session bootstrap), `tvnext-world` (bevy_ecs world + reactor + backend bridge; headless tests), `tvnext-daemon` (zenoh bridge + boot + wire tests), `tvnext-cli` (zenoh client).
