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
4. **Vendor-agnostic seam.** A generic "device reports its link MAC" read on the connection; LG implements it, CEC keeps the default.
5. **Wizard hint, composed.** Where ARP finds nothing, the wizard prepends "MAC not detected." to the vendor's own MAC helper, and LG's helper says the MAC is read from the TV after pairing if left blank.
6. **Superseded dials are dropped** (dial generation), which also closes a pre-existing re-pair race (below).

Out of scope: a TV with more than one address (the stack stays single-homed: one dial address, one `mac`); refreshing the MAC after pairing; an SSAP-sourced MAC for the manual-entry enrichment preview; any use of `isWakeOnWifiEnabled`.

## Design

### Core (`tisvlay-core`)

- `TvConnection::get_link_mac(&mut self) -> Option<MacAddress>`, async, default `None`: "the MAC of the interface this connection reached the TV through, as the device reports it". The `get_` prefix follows the trait's convention for async device reads (`get_volume`, `get_current_input`); plain nouns (`hardware_id`, `brand`) are cached sync reads. Trait docs: best-effort, and implement it only if the backend's config `mac` drives wake.
- Deliberately **not** a `HardwareId` field. `HardwareId` is the published EDID-matching identity; a link MAC is a network fact with different consumers.
- A named constant for the cross-vendor config key `mac` (today a string literal in the bridge's enrichment, the wizard and the schema form), used by the world's fill and those sites.
- `MacAddress::parse` hardened against non-ASCII input. It slices bytes today and can panic on a 12-byte non-ASCII string; check `is_ascii_hexdigit` first.

### LG (`tisvlay-lg`)

- `WebOsClient::get_link_mac(dialed: IpAddr) -> Option<MacAddress>`: read `getstatus`, take the interface (`wired` or `wifi`) whose `state == "connected"` and whose `ipAddress` parses to `dialed`, then read `getinfo` and return that interface's `macAddress`, parsed. `None` (logged at debug) on an error response, `returnValue: false` (a Luna failure can arrive as a normal response), an unmatched IP, or a missing block. Never `p2pInfo`.
- Bypasses the connection's reconnect-on-timeout wrapper: it's a best-effort read, not a command to retry.
- `LgConnection::get_link_mac` parses `config.ip` as an `IpAddr` and returns `None` for a hostname or an IPv6 dial (the field sample only shows a v4 `ipAddress`), otherwise delegates.
- Endpoint constants for both URIs in `endpoints.rs`.
- `connector.rs` MAC field helper: "Required for Wake-on-LAN. Leave blank to read it from the TV after pairing."

### World (`tisvlay-world`)

**Pair-time read.** `Effect::SpawnConnect` gains `pairing: bool`, set by `reconcile_connections` when the TV carries `PairVerifying`. The world states the policy ("this is a verification dial") and `connect_flow` decides which pair-time reads to do: it calls `connection.get_link_mac()` under a ~3 s `tokio::time::timeout` and carries the result on `BackendEvent::Connected { link_mac: Option<MacAddress>, .. }`. A slow or silent TV costs at most the timeout before `complete`, never the 10 s × 2 request budget.

Every dial during verification carries the flag. `PairVerifying` and its `PendingConnect` land in one command flush, verification dials are always plain (never wake), and verification ends on the first `ConnectFailed`, so no retry, wake dial or carrier redial is ever a pairing dial.

**Dial generation.** Today a re-pair's `ConfigReady` inserts `PairVerifying` and calls `redial`, but doesn't cancel a dial already in flight, and `reconcile_connections` skips entities that are `Reconnecting`. That old dial (old config, no `pairing` flag) can land afterwards and publish `complete`, with no report and **having verified the old credentials**. That's a pre-existing bug, and likely common: a re-pair usually happens when the old key is failing.

- `Reconnecting` gains `dial: u64`, taken from a world `DialSeq` counter when `reconcile_connections` spawns the dial.
- `Effect::SpawnConnect` carries `dial`; `connect_flow` echoes it on `BackendEvent::Connected` and `BackendEvent::ConnectFailed`. Later events (`State`, `Disconnected`, `CommandDone`) ride the live handle and need no tag.
- The world applies a `Connected` / `ConnectFailed` only when its `dial` matches the entity's current `Reconnecting.dial`. A stale `Connected` is dropped, and with it its `cmd_tx`, which ends that backend task and closes its connection.
- Re-pair's `ConfigReady` removes `Reconnecting`, so the old dial is superseded and the verification dial spawns immediately.

**Fill rule.** In the `Connected` handler's `PairVerifying` branch, before publishing `complete`:

- The TV's config `mac` is "unset" when the key is absent or a string that's empty after `trim()`. An existing `wake_id` key (the serde alias of LG's `mac`) counts as set: writing `mac` next to it would make the config fail to parse with a duplicate field.
- Unset and `link_mac` is `Some`: write `mac` as the canonical `MacAddress` Display form (uppercase, colons), mark the store dirty, and push `Effect::Facts` so `supports_wake`, the transport traits and the TVs-pane rows refresh now rather than at the next dial. Share that facts-refresh step with `apply_config_patch` as a helper, without its redial half: the TV was just verified.
- `apply_backend_events` gains a mutable `VendorRef` query.

### Wire (`tisvlay-wire`)

`PairingProgress` gains an optional, `complete`-only `link_mac` (`#[serde(default, skip_serializing_if = "Option::is_none")]`):

```rust
pub struct LinkMacReport {
    /// The device-reported MAC, canonical form.
    pub reported: String,
    pub outcome: LinkMacOutcome,
}

pub enum LinkMacOutcome {
    /// `mac` was unset; the daemon stored `reported`.
    Filled,
    /// The configured `mac` already equals `reported`.
    Matches,
    /// The configured `mac` differs and was kept.
    Differs { configured: String },
}
```

The daemon does the comparison, through `MacAddress` so case and separators don't count, and clients only render it. It's a snapshot at terminal time: the cached `complete` sample goes stale once the user accepts **Use reported**. It's absent when the vendor doesn't report a MAC or the read failed or timed out. Scoping to the pairing `attempt` is already handled by the per-session attempt id.

### SDK (`tisvlay`)

- `PairingProgressDetail` gains `link_mac: Option<LinkMacReport>`, parsed from `progress["link_mac"]` in `pairing_progress_stream`; the struct derives `Default`, so test helpers keep compiling.
- `drain_pair_to_terminal_pinned` copies it into `PairTerminal::Complete { link_mac, .. }`.

### GUI (`tisvlay-gui`)

- **Step 2 (Configure):** `apply_manual_entry_helper_overrides` stops replacing the MAC helper. It prepends a translated "MAC not detected." to the vendor's own helper, so the wizard stays vendor-blind and each vendor's text says what that vendor can do.
- **Plumbing:** `terminal_to_result` keeps `Complete`'s `link_mac` instead of discarding it, and `publish_pair_outcome` carries it to step 4, for both pair and re-pair.
- **Step 4 (Done):**
  - `Filled`: "MAC detected from the TV: `<reported>`".
  - `Differs`: "The TV reports `<reported>` for its active connection, but `<configured>` was entered." with **Use reported** (sends `update-config {"mac": reported}`) and **Keep mine** (dismisses). Accepting causes a brief reconnect, because `update-config` redials on a vendor change; a failed write shows inline in step 4.
  - `Matches` or absent: nothing.
  - Strings via `gettext` / `@tr`; run `just i18n-extract`.
- The decision from the report to what step 4 shows is a pure function, unit-tested.

### CLI (`tisvlay-cli`)

`watch_pairing` prints, on `complete`: `mac: detected <reported> (saved)`, or `mac: TV reports <reported>, configured <configured>; to use it: tisvlay update-config <id> '{"mac":"<reported>"}'`.

## Tests

- **LG client unit tests:** `FakeSsapServer` gains a per-test `set_command_payload(uri, Value)` override next to the existing error override. Cases: wired-connected (fixture includes a `p2pInfo` block, which must never be chosen), Wi-Fi-connected, IP mismatch, 404, `returnValue: false`. Plus `LgConnection` returning `None` for a hostname or IPv6 config without calling the TV.
- **Mock backend:** a `link_mac` knob in `MockVendorConfig`, following the existing `wake` / `refuse_connect` knobs, so the daemon's wire test can set it through the `pair` rpc, plus a `MockHandle` link-MAC read counter, following `connect_call_count`.
- **World unit tests** (`pairing_profiles.rs`, on the mock):
  - unset `mac` + report → filled, `Filled`, and `Link.supports_wake` flips true
  - differing → untouched, `Differs`
  - equal → `Matches`
  - no report → no block
  - `wake_id` present → treated as set
  - a plain (non-pairing) dial never reads (counter stays 0)
  - **dial generation:** a stale `Connected` arriving after a re-pair's `ConfigReady` is dropped and doesn't publish `complete`; the verification dial's outcome does
- **Wire test** (daemon `wire.rs`): a mock pair with a `link_mac` knob; the terminal carries `Filled` and the persisted config holds the canonical MAC.
- **SDK drain test:** `Complete` carries the report, using the existing `sample()` helper.
- **GUI:** the step-4 decision function (Filled / Matches / Differs / absent).

## Commit sequence

Each commit compiles and carries its first use.

1. **core + mock + world: dial generation.** `DialSeq`, `Reconnecting.dial`, the echo on `Connected` / `ConnectFailed`, re-pair superseding, stale-drop test. A standalone fix for the old-credentials race.
2. **core + mock + world: pair-time link MAC.** `get_link_mac` with a `None` default, the hardened `MacAddress::parse`, the `mac` key constant, the mock knob and counter, `SpawnConnect.pairing`, the bounded read in `connect_flow`, the fill rule with the shared facts refresh, and the world tests.
3. **LG.** Endpoint constants, `WebOsClient::get_link_mac`, `LgConnection` implementation, the fake-server override and its tests, the new MAC helper text, and the composed wizard override, plus `just i18n-extract`.
4. **Reporting.** `LinkMacReport` on `PairingProgress`, world stamping, SDK detail and `PairTerminal`, CLI output, the daemon wire test and the SDK drain test.
5. **GUI step 4.** The plumbing through `terminal_to_result` / `publish_pair_outcome`, the decision function and its test, the detected / mismatch UI, and `just i18n-extract`.
6. **Docs.** Migrate per below, delete this plan.

## Docs on merge

Per [README.md](README.md), with destinations corrected for where the content fits today:

- `docs/architecture.md`: fold the fill rule, the pair-time read and the dial generation into the existing `PairVerifying` paragraph.
- `docs/discovery.md`: rewrite the follow-up sentence in § "Manual-entry MAC enrichment", which still names pair-time ARP snooping as the fix.
- `docs/backends/implementing-a-vendor.md`: the optional `get_link_mac` seam.
- `docs/ideas/pair-time-mac-capture.md`: trim the device-reported section to a pointer and keep the remaining pair-time-ARP content; trim its title in `docs/ideas.md`.
- Field facts are already in `docs/lg/firmware-quirks.md`.
- Remove this plan's entry from `docs/plans/README.md`, delete the plan, and sweep inbound references.
