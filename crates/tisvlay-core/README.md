# tisvlay-core

Vendor- and transport-agnostic abstractions for controlling TVs.

This crate defines the traits a TV backend implements, the capability flags it advertises, the pairing state machine, and the shared state, config and error types. Vendor implementations live in their own crates: `tisvlay-lg` (LG webOS over SSAP) and `tisvlay-cec` (HDMI-CEC through the Linux CEC subsystem). The `tisvlay` daemon registers them side by side, and it never sees a transport-specific type.

## Architecture

```text
tisvlay-core (this crate)
├── traits: TvBackend, TvConnection, TvConnector, TvDiscovery
├── capabilities: Capability flags for feature detection
├── pairing: Unified pairing state machine
├── config: Profile-based configuration
├── state: TV state types
├── error: Vendor-agnostic errors
├── secret: Debug-redacted credential values
├── vendor_config: the vendor-specific config table
└── wol: Wake-on-LAN (generic)

tisvlay-lg, tisvlay-cec (separate crates)
└── vendor implementations of the traits
```

A backend's transport details (an IP address, a CEC device node, a client key) live in its `VendorConfig`, a TOML table that only the vendor crate interprets. Each vendor crate offers a typed config struct that converts to and from it.

## Stability

Error enums and their code enums, `PowerState`, and the records that may grow (`TvState`, `Credentials`) are `#[non_exhaustive]`, so a new variant or field is not a breaking change: match them with a wildcard arm. Enums whose every arm matters to a caller (`AutomateWhen`, `PairingState`, `Transport` and the like) stay exhaustive, so a new variant is a compile error where it needs handling.

## Public dependencies

These crates' types appear in this crate's API, so a major-version bump of one is a breaking change here:

- `tokio`: `TvConnection::subscribe_state` returns an unbounded `mpsc` receiver, and `PairingSession::subscribe` a `watch` receiver.
- `uuid`: profile references and the default-profile constants are `Uuid`s.
- `bitflags`: `Capabilities` is a `bitflags` type.
- `serde`: the config, state and pairing types implement `Serialize`/`Deserialize`, and `VendorConfig` converts through them.
- `toml`: `VendorConfig` wraps a `toml::Table`, and `Error::Config` carries a `toml::de::Error`.
- `serde_json`: `Error::Json` carries a `serde_json::Error`.
- `async-trait`: `TvConnection` and `TvConnector` are `#[async_trait]` traits, so their method signatures are that macro's expansion, and implementations apply the macro too.

## License

Apache-2.0.
