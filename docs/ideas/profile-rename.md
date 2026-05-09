# Profile rename

System and user profiles can be created, edited, deleted today, but not renamed. Rename was scoped out of the M3.5 / M4 profile-CRUD work because it has to walk every TV's `profile.system` / `profile.user` assignment to rewrite atomically — a meaningfully different operation from the per-profile CRUD methods.

Path forward when this becomes a concern:

- Daemon-side `Manager1.RenameSystemProfile(old: s, new: s)` and `Session1.RenameUserProfile(old: s, new: s)` that do the assignment-rewrite under an internal lock.
- Validation: same alphabet rules as create (`[a-z0-9-]+`), `default` undeleteable so also unrenameable.
- Atomicity: load → rewrite all references → save in one disk-write window, same `disk_write_lock` pattern the existing CRUD uses.
- GUI-side: rename input on the profile editor with the same validation feedback the new-profile flow has.

Skipped in v1 simply because no user has asked for it; the create-new-then-reassign-then-delete-old workflow covers the same need with three clicks.

