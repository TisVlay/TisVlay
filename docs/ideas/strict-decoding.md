# Strict decoding of unknown fields

Every serde type TisVlay decodes, from rpc requests to state files, skips fields it doesn't know. No type sets `#[serde(deny_unknown_fields)]`, so an old or misspelled field name is dropped without a word. Found 2026-10-25, after the stream rename: pox's session store still carried `remote_screencast_arms`, which the renamed session now ignores (harmlessly here, since its value was the default).

The project's rule is "no compatibility shims", and nothing was added for compatibility: serde's default leniency has been there all along. But it acts like a silent shim: a rename resets a persisted setting to its default on upgrade, and a typo in a request is accepted and does nothing.

## What happens today

| Surface | Types | An unknown field |
|---|---|---|
| rpc requests | `rpc::*`, the profile bodies inside `ProfileCreate` / `ProfileUpdate` / `UserProfileCreate` / `UserProfileUpdate` | ignored: `tisvlay profile system create X --body '{"on_tv_power_of":"lock"}'` succeeds and sets nothing |
| The daemon's store | `TvRecord`, `ProfileRecord`, `SystemProfileConfig` | ignored, so a renamed field falls back to its default |
| The session's store | `UserStoreSnapshot` (`UserProfileRecord`, `Assignment`, `UserProfileConfig`) | the same |
| The tray's settings | `PersistedSettings`, which flattens `NotificationSettings` under its schema `version` | the same |
| The seed config | `SeedConfig` | the same |
| Vendor configs | `VendorConfig`, parsed by each backend (`CecVendorConfig`, `LgVendorConfig`, `MockVendorConfig`) | ignored, and relied on: a re-pair hands `start_pairing` a merged config whose extra fields the typed config doesn't declare (the mock's `start_pairing_accepts_credentialed_vendor_config` pins this) |

Values are strict already: an unknown enum token (`"on_tv_power_off": "hibernate"`) or a wrong type is refused. That's what `docs/architecture.md`'s "requests decode strictly" and the project instructions' "Requests decode strictly" mean today; neither covers unknown fields, and both read as if they did.

## What strictness would touch

- **`#[serde(flatten)]` can't combine with `deny_unknown_fields`.** The tray's `PersistedSettings` would need restructuring, or a hand-written check.
- **`EventAction` is `#[serde(untagged)]`.** With the `With` variant denying unknown fields, a table with a typo fails both variants, and serde reports only "data did not match any variant", not which field. A good error likely needs a hand-written `Deserialize`.
- **Vendor configs depend on leniency** (above): a strict parse would need each backend to declare every key it can be handed, or the merge to strip the ones it doesn't own.
- **A strict store refuses to load.** The daemon would exit and its unit restart it in a loop until the file is fixed by hand; the session would come up without its profiles. `reload` already refuses a bad store with an error, which is the shape a load failure would want too.
- **Skew.** The architecture accepts that a newer daemon and an older user service may refuse each other until re-login. Strict responses (a client refusing a reply with a field it doesn't know) would widen that window; strict requests wouldn't change it.

## Open questions

1. **Which surfaces?** Requests only (typos fail loudly, stores keep loading), stores too (renames fail loudly), or vendor configs as well?
2. **How should a strict store fail?** Refuse to start, with an error naming the file and the field? Or start without the bad record and report it (an `event/` key, a GUI banner)? The first matches "no shims"; the second keeps TV control up.
3. **Renames.** The vocabulary's renames after the TV-controls-the-PC campaign rename persisted fields ([power-vocabulary.md](power-vocabulary.md)). Lenient decoding makes them silently reset users' settings; strict decoding makes them break stores loudly. Which does the project want, and does either call for a rename note in the release notes, given no migration code?
4. **Error quality.** Is a hand-written `Deserialize` for `EventAction` worth it, or is a generic serde error naming the line acceptable?
5. **Responses.** Should replies stay lenient, so the skew window doesn't widen, while requests and stores go strict?
6. **Where to validate.** The GUI only writes fields it knows; the CLI's `--body` and hand-edited stores are where unknown fields come from. Is decode-time strictness the right place, or a separate lint (`tisvlay reload --check`)?

**Trigger:** before the vocabulary's post-campaign renames, since they rename persisted fields and this decides whether that's silent or loud.
