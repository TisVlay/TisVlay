## zenoh pins an lz4_flex with a decompression advisory

**Priority**: low
**Found by**: GitHub's Dependabot alert 1 on `Cargo.lock` (opened 2026-10-24; dismissed 2026-11-01 as "vulnerable code is not actually used")

### Context

The advisory: lz4_flex before 0.11.6 can leak uninitialized memory, or a reused output buffer's old contents, when decompressing. The lockfile has lz4_flex 0.10.0, pulled in by `zenoh-transport` 1.10, which requires 0.10. The fixed 0.11 line is semver-incompatible, so no bump reaches it until zenoh moves.

zenoh calls lz4_flex only behind its `transport_compression` feature. The workspace builds zenoh with `default-features = false` and `transport_unixsock-stream` alone (the root `Cargo.toml`), so the crate is compiled in but its compression and decompression never run. `cargo deny`'s advisories check (`just audit`) passed on 2026-11-01, as the advisory database didn't list it then.

### Risk

None while compression stays off. Turning it on, directly or through a zenoh feature that implies it, would make the decompression reachable on every zenoh link.

### Mitigation options

- **Bump zenoh** once a release depends on lz4_flex 0.11.6 or later; this doc then retires.
- **Keep compression off** until then. If a zenoh bump needs it, check the lz4_flex version first.

### Re-evaluation triggers

- A zenoh bump.
- Enabling `transport_compression`, or any zenoh feature that implies it.
- The advisory reaching the RustSec database, so `just audit` fails: add an ignore to `packaging/licensing/deny.toml` that points here.
