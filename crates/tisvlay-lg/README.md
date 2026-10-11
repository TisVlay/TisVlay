# tisvlay-lg

LG webOS TV backend for tisvlay.

This crate implements the `tisvlay-core` traits for LG webOS TVs. It speaks SSAP (Simple Service Access Protocol) over a WebSocket: `wss://` on port 3001 with the TV's self-signed certificate, which current firmware requires.

## Features

- Power on (Wake-on-LAN) and off
- Screen on/off (OLED-safe blanking, the TV stays on)
- HDMI input switching
- Volume and mute control
- Backlight control (OLED pixel brightness)
- Pairing: the two-phase on-TV prompt that issues a client key
- Network discovery via SSDP, behind the `discovery` feature

## Example

```rust,no_run
use tisvlay_core::{TvConnector, VendorConfig};
use tisvlay_lg::{LgConnector, LgVendorConfig};

#[tokio::main]
async fn main() -> tisvlay_core::Result<()> {
    let mut config = LgVendorConfig::new("192.168.1.100");
    config.mac = Some("AA:BB:CC:DD:EE:FF".into());
    config.set_client_key("your_client_key");
    let config = VendorConfig::from_typed(&config).expect("an LG config is a TOML table");

    let mut connection = LgConnector::new().connect(&config).await?;
    connection.set_volume(20).await?;
    connection.close().await?;
    Ok(())
}
```

The SSAP client and protocol types are private: firmware moves the protocol around, and keeping it out of the API means those changes never break a caller.

## Public dependencies

- `tisvlay-core`: the traits this crate implements and the types they carry. The tisvlay crates release in lockstep, so use matching versions.
- `serde`: `LgVendorConfig` implements `Serialize`/`Deserialize`, which is how it converts to and from a `VendorConfig`.

## License

Apache-2.0.
