# Fuzzing the parsers

Each parser of outside input already has a "never panics on arbitrary input" property test (proptest), run on stable in every `just check`. Those explore a few hundred random cases per run. A coverage-guided fuzzer explores far deeper, steering toward inputs that reach new branches.

## How

[bolero](https://crates.io/crates/bolero) runs one harness both ways: as a property test on stable, and as a libFuzzer or AFL target on nightly. So the existing properties can grow into fuzz targets without a second harness:

- `tisvlay-ssdp` `decode_datagram` / `parse_response`
- the daemon's EDID `parse_hardware_id`
- lg's SSAP decoding (`parse_power_state_payload`, the protocol types)
- cec's input-id parsing and OSD-name clamping
- core's `MacAddress::parse`

The fuzz runs need nightly. That fits a separate, occasional job, never the stable gates or the dev pin.

## Trade-offs to budget for

- **A nightly toolchain for the fuzz runs:** fine as a one-off job, but it's a second toolchain to keep fetchable.
- **Corpus upkeep:** fuzzing pays off over long runs that keep a corpus; one-off runs find shallow bugs only.

## Trigger conditions

- A parser panic or misparse reaches a user despite the property tests.
- A new parser of untrusted bytes lands (e.g. another vendor's discovery protocol).
