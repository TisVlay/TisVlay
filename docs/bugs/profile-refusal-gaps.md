## Profile refusals: wrong wording, wrong code, a missing check

**Status**: open
**Severity**: low — each one misleads a caller, none loses data
**Found by**: documenting every public item of the protocol and the SDK

### Problem

Three small gaps in how the services refuse profile rpcs:

- **"System profile" for user profiles.** `WireError`'s `profile_*` constructors (`crates/tisvlay-protocol/src/error.rs`) say "no system profile with id …", "a system profile with id … already exists" and "system profile … is referenced by at least one TV". The session service returns the first two for user profiles too, so a user-profile refusal names the wrong kind.
- **`BadRequest` for an internal failure.** The session service answers "the store writer is gone" (`crates/tisvlay-session/src/lib.rs`) as `bad-request`, which tells the caller its request was malformed. It's the service's own failure, so it wants an internal code.
- **`tv::SetSystemProfile` doesn't check the profile exists.** `set_system_profile` (`crates/tisvlay-world/src/systems/commands.rs`) stores any id, and a TV assigned an unknown profile silently follows the default. It should refuse with `profile_not_found`, like the profile rpcs do.

### Fix sketch

Take the profile kind in the `profile_*` constructors (or word them kind-neutrally), add or reuse an internal error code for the session's store failure, and look the id up in the profile store before `set_system_profile` writes it. Each is a wire-visible change, so it ships as a `feat`.
