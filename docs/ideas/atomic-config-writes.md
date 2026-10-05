# Atomic config writes (tempfile + rename)

The daemon store, the session store and the tray's settings each write `<path>.tmp` and `rename(2)` it over the live file, so an interrupted write never leaves a torn config. None of them `fsync`s the temp file before the rename, though, so a power loss right after a save can still leave an empty file under the live name on some filesystems. The full pattern is "write to `<path>.tmp`, fsync, `rename(2)`".

Deferred because no failure mode has been observed; an interrupted save typically affects a single edit which the user can redo. Worth doing when:

- The first user reports a config that won't load after a crash, or
- A new write site lands that's harder to diagnose post-crash than "rerun the last edit", or
- We add an automated edit pipeline (e.g., GUI batch-edits) where the user wouldn't easily know which edit was in flight.

Implementation is small (~20 LOC: one shared `write_atomic(path, contents)` helper with the fsync, replacing the three copies).
