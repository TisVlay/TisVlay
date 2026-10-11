# tisvlay-cec

HDMI-CEC TV backend for tisvlay.

This crate implements the `tisvlay-core` traits over the HDMI Consumer Electronics Control bus, through the Linux CEC subsystem (`/dev/cecN`). It's built on Valve's `linux-cec` library. A host needs a CEC adapter: a USB adapter, or a GPU output whose driver exposes DisplayPort-CEC.

Unlike the network backends, the adapter stays reachable on the bus while the TV is in standby. Power-on is a real on-bus command (`Image View On`) rather than Wake-on-LAN, and a standby TV stays *connected*: callers judge whether it's on by its reported power state, not by whether a connection succeeds.

## Capabilities

- Power on, off, and power-state queries. Powering on also makes the adapter the active source, so the TV switches to the PC's input.
- Optionally, screen blanking emulated by powering the TV (`emulate_screen_via_power`), since CEC has no discrete screen-off
- Advertising an OSD name, the label the TV shows for the PC's input
- Discovery of the TVs on each `/dev/cecN` bus

Switching to an arbitrary input is not advertised: `Set Stream Path` only works when a CEC device answers on the target port, and CEC can't read back the current input. Volume over CEC is relative-only and not offered. Backlight is not a CEC concept.

## Example

```rust,no_run
use tisvlay_cec::{CecConnector, CecVendorConfig, PhysicalAddress};
use tisvlay_core::{TvConnector, VendorConfig};

#[tokio::main]
async fn main() -> tisvlay_core::Result<()> {
    let config = CecVendorConfig {
        device: "/dev/cec0".into(),
        // The adapter sits on the TV's HDMI 2.
        physical_address: "2.0.0.0".parse().expect("a valid physical address"),
        ..CecVendorConfig::default()
    };
    assert_eq!(config.physical_address, PhysicalAddress::new(0x2000));
    let config = VendorConfig::from_typed(&config).expect("a CEC config is a TOML table");

    let mut connection = CecConnector::new().connect(&config).await?;
    connection.power_on().await?;
    connection.close().await?;
    Ok(())
}
```

## Public dependencies

- `tisvlay-core`: the traits this crate implements and the types they carry. The tisvlay crates release in lockstep, so use matching versions.
- `serde`: `CecVendorConfig` and `PhysicalAddress` implement `Serialize`/`Deserialize`, which is how the config converts to and from a `VendorConfig`.

`linux-cec` is not part of the API. The `test-link` feature is the one exception: it exposes scripted link doubles for out-of-crate tests, along with linux-cec's `PowerStatus` and `VendorId` that they speak. That feature is outside this crate's semver guarantees.

## License

Apache-2.0.
