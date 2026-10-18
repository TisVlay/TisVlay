# SDK methods for the rpcs the GUI can't reach

Three rpcs have a typed endpoint but no method on the client SDK, so the GUI can't use them:

- **`manager/rpc/add-tv`**: enroll a TV from a full config without pairing. No client calls it (the daemon and CLI tests do); the GUI and the CLI only pair.
- **`manager/rpc/discover`**: the bounded scan that streams one reply per finding (the CLI's `discover`). The GUI's wizard uses the open-ended `discover-start` session instead.
- **`user/profile/rpc/effective`**: one TV's resolved user-profile mapping (the CLI's `user-profile <tv> --effective`). The GUI shows the system profile's effective mapping but not the user profile's.

Each is a few lines now: a thin method over `Caller::call::<R>` (or `call_many` for `discover`), with the endpoint's request and response types already in place.

## Trigger conditions

- A GUI feature needs one: enrolling a TV with a known client key, a one-shot scan, or showing which user-profile actions apply to a TV.
