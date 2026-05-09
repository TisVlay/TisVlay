# Atomic config writes (tempfile + rename)

Today's `SystemConfig::save_to` is a synchronous `fs::write` — an interrupted write can leave the on-disk config half-rewritten. The standard Linux pattern is "write to `<path>.tmp`, fsync, `rename(2)` over the live file" so a crash mid-write leaves either the old or the new content but never a torn one.

Scoped out of the Phase 8.1 UUID work because no failure mode has been observed; an interrupted save typically affects a single edit which the user can redo. Worth doing when:

- The first user reports a config that won't load after a crash, or
- A new write site lands that's harder to diagnose post-crash than "rerun the last edit", or
- We add an automated edit pipeline (e.g., GUI batch-edits) where the user wouldn't easily know which edit was in flight.

Implementation is small (~20 LOC: extract a `write_atomic(path, contents)` helper, swap call sites).
