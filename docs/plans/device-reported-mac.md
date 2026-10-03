# Device-reported MAC at pair time

Fill a paired TV's Wake-on-LAN MAC from the TV itself, so TVs off the daemon's link (a routed or VLAN-isolated IoT segment, where ARP only ever sees the gateway) get WoL without the user hunting down a MAC, and flag a submitted MAC that disagrees with what the TV reports.

Origin: [docs/ideas/pair-time-mac-capture.md](../ideas/pair-time-mac-capture.md) § "Device-reported MAC". Field case: the OLED42C3PUA on a reflector-bridged IoT VLAN (SSDP and WoL reflected across), where the wizard auto-filled the IP but never the MAC.

## Field findings this rests on

Verified on the OLED42C3PUA; the OLED55G3PUA shares the firmware family. Full detail in [docs/lg/firmware-quirks.md](../lg/firmware-quirks.md).

- `ssap://com.webos.service.connectionmanager/getinfo` returns one MAC per interface: `wiredInfo` / `wifiInfo` / `p2pInfo`.
- `ssap://com.webos.service.connectionmanager/getstatus` (lowercase) returns per-interface `state`, `interfaceName` and `ipAddress`. Joining the two identifies the attached NIC exactly: the connected interface whose IP equals the dialed address.
- Both need `READ_NETWORK_STATE`, which the pairing manifest already requests.
- `wifi.isWakeOnWifiEnabled` reads `false` while Wi-Fi WoL demonstrably works (three wakes across the VLAN), so it's not used.

## Decisions (from review with the maintainer)

1. **Fill if empty; never overwrite.** A blank `mac` is filled from the TV. A non-empty `mac` (typed or ARP-auto-filled) is never changed by the daemon.
2. **Flag a mismatch at pair time, in the wizard.** When a submitted MAC differs from the TV's, the wizard's finished step says so and offers **Use reported** / **Keep mine**. No persistent state, no TVs-pane hint, nothing that has to survive a restart.
3. **Query at pair / re-pair only**, not on every connect.
4. **Vendor-agnostic seam.** A generic "device reports its link MAC" capability on the connection; LG implements it, CEC (and the mock, by default) don't.
5. **Wizard hint.** Where ARP finds nothing, the MAC field's helper says it'll be read from the TV after pairing, for vendors that support it.

Out of scope: a TV with more than one address (the stack stays single-homed: one dial address, one `mac`); refreshing the MAC after pairing; the SSAP-sourced MAC for the CLI's manual-entry enrichment preview; any use of `isWakeOnWifiEnabled`.

## Design

### Core (`tisvlay-core`)

- `TvConnection::link_mac(&mut self) -> Option<MacAddress>`, async, default `None`: "the MAC of the interface this connection reached the TV through, as the device reports it". The default keeps every existing backend compiling and opted out.
- Deliberately **not** a `HardwareId` field. `HardwareId` is the EDID-matching identity (manufacturer / model / serial) and is published as such; a link MAC is a network fact with different consumers.

### LG (`tisvlay-lg`)

- `WebOsClient::get_link_mac(dialed_ip)`: read `getstatus`, take the interface (`wired` or `wifi`) whose `state == "connected"` and `ipAddress == dialed_ip`, then read `getinfo` and return that interface's `macAddress`. `None` (logged at debug) on any 404/401, an unmatched IP, or a missing block. Never `p2pInfo`.
- Endpoint constants for both URIs in `endpoints.rs`.
- `LgConnection::link_mac` delegates to the client with the configured IP.
- Fake SSAP server: canned `getinfo` / `getstatus` responses (connected wired, connected Wi-Fi, IP mismatch, 404) for the client tests.
- `connector.rs` MAC field: helper text gains the post-pair detection note.

### World (`tisvlay-world`)

- `Effect::SpawnConnect` gains `read_link_mac: bool`, set when the dialed TV carries `PairVerifying`, which is exactly the fresh-pair and re-pair verification dials. `connect_flow` calls `connection.link_mac()` only when it's set, and carries the result on `BackendEvent::Connected` (`link_mac: Option<MacAddress>`).
- In the `Connected` handler's `PairVerifying` branch, before publishing the terminal `complete`:
  - If the TV's vendor config has no `mac` (absent or blank) and `link_mac` is `Some`: write it into the config, mark the store dirty (the existing config-publish path follows).
  - Stamp the terminal with what happened (below).
- The generic config key `mac` is already the cross-vendor name the bridge's manual-entry enrichment writes (`enrich_config_mac`), so the world writing it isn't a new vendor coupling.

### Wire (`tisvlay-wire`)

`PairingProgress` gains an optional, `complete`-only `link_mac` block:

```rust
pub struct LinkMacReport {
    /// What the device reported for the attached interface.
    pub reported: String,
    /// The TV's `mac` after the fill rule ran (empty if none).
    pub stored: String,
    /// The daemon filled `stored` from `reported` on this pairing.
    pub filled: bool,
}
```

Absent when the vendor doesn't report a MAC or the read failed. The wizard derives everything from it: `filled` → "detected"; `!filled && stored != reported` (normalized) → mismatch; equal → nothing. MAC comparison normalizes through `MacAddress::parse`, so case and separator differences don't count as a mismatch.

### SDK (`tisvlay`)

`PairTerminal::Complete` gains `link_mac: Option<LinkMacReport>`, populated by the drain from the terminal sample.

### GUI (`tisvlay-gui`)

- **Step 2 (Configure):** the manual-entry helper override for an empty MAC, `apply_manual_entry_helper_overrides`, currently replaces the schema helper with "MAC not detected. Enter it manually…". It has to stop contradicting the vendor's note; see open question 1.
- **Step 4 (Done):** from the terminal's `link_mac`:
  - `filled`: "MAC detected from the TV: `<reported>`".
  - mismatch: "The TV reports `<reported>` for its active connection, but `<stored>` was entered." with **Use reported** (sends `update-config {"mac": reported}`) and **Keep mine** (dismisses).
  - Strings via `gettext` / `@tr`; run `just i18n-extract`.
- Re-pair uses the same step 4, so a re-pair surfaces a mismatch against the TV's current `mac`.

### CLI (`tisvlay-cli`)

`watch_pairing` prints, on `complete`: `mac: detected <reported> (saved)`, or `mac: TV reports <reported>, configured <stored>; to use it: tisvlay update-config <id> '{"mac":"<reported>"}'`.

## Tests

- **LG client unit tests** (fake server): wired-connected, Wi-Fi-connected, IP-mismatch → `None`, 404 → `None`, `p2pInfo` never chosen.
- **World unit tests** (`pairing_profiles.rs`, mock backend with a settable link MAC): empty `mac` + report → filled, terminal `filled: true`; non-empty differing → untouched, terminal reports both; equal → no flag; no report → no block; a plain (non-verifying) dial never calls `link_mac`.
- **Wire test** (daemon `wire.rs`): a mock pair whose terminal carries `link_mac`, and the persisted config holding the filled MAC.
- **SDK drain test**: `Complete` carries the report.
- **GUI**: the step-4 decision function (filled / mismatch / equal / absent) as a pure unit test, in the style of `picked_tv_name`.

## Docs on merge

Per the plans README: decisions to `docs/architecture.md` (pairing section) and `docs/discovery.md` (§ MAC enrichment: the post-pair path); field facts are already in `docs/lg/firmware-quirks.md`; trim the idea doc's device-reported section to a pointer and keep its remaining pair-time-ARP content. Delete this plan.

## Open questions for review

1. **The step-2 helper.** The wizard-side override is vendor-blind, but "read from the TV after pairing" is only true for vendors that implement `link_mac`. Options: (a) the override keeps the schema helper when the vendor declares post-pair detection, which needs that fact in the vendor catalog; (b) neutral wording ("…or leave blank if your TV reports it after pairing"); (c) drop the override for `mac` and let each vendor's schema helper speak.
2. **Mock backend.** Should the mock implement `link_mac` (settable per test) to exercise the world path end to end, or should the world tests use a test-only connection?
3. **Wizard "Use reported" from re-pair.** Re-pair has no step-2 form; is an `update-config` from step 4 the right path there too? It reuses the existing rpc, so it should be.
