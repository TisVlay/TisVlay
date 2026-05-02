# Plan 5 — Power-cycle robustness, MAC capture, vendor-agnostic config

> **Status (mostly complete; one refinement queued):** The
> office-TV silent-fail bug's full repair chain is live end-to-end:
> typed errors surface; daemon ARPs the MAC from the kernel after
> each SSDP hit and stuffs it into `vendor_data`; the GUI wizard's
> step-2 form renders the LG schema dynamically with autofill from
> `vendor_data`, client-side validation lifecycle, server-side
> error routing, and a Kirigami inline-error surface on the TVs
> pane. PowerOn probes direct-connect first and only falls through
> to WoL when the TV is unreachable; the Power-on button disables
> with an explanation when the TV is offline AND has no wake
> method.
>
> Real-world testing on `feat/gui` surfaced two refinements past
> the original plan: the LG backend's `connect()` was burning
> ~25 s in nested retry loops on cold TVs (now fixed — see
> "Phase 6 follow-ups" section below), and the direct-connect
> probe still costs a 5 s TLS handshake timeout when the TV was
> just powered off (queued).
>
> Out-of-scope follow-ups (Phase 7 "Open logs", Phase 8
> "Edit TV properties") are filed for future PRs.
>
> | Phase | Done | Commits |
> |---|---|---|
> | **0** — i18n scaffold | ✅ | `bcd39b7` |
> | **4a** — bug-fix surfacing in PowerOn | ✅ | `dcba418` |
> | **4b** — `PowerError` + per-domain skeleton | ✅ partial (Power only) | `dac954f` |
> | **4b** — adopt `PowerError` in `TvManagerError` + `DaemonError` | ✅ | `9bfc2b3` |
> | **1** — schema types + `TvConnector::pairing_schema()` | ✅ | `50947dc` |
> | **1** — LG `pairing_schema()` impl | ✅ | `8c3bafa` |
> | **1** — `Manager1.GetPairingSchema(vendor)` D-Bus method | ✅ | `d4c739e` |
> | **1** — schema-driven `AddTv` validation | ✅ | `7fd59b7` |
> | **3** — `vendor_data` field on `DiscoveredTv` | ✅ | `b7c718b` |
> | **3** — `netlink::arp_lookup` helper | ✅ | `141cf43` |
> | **3** — daemon enriches discovery with ARP-resolved MAC | ✅ | (ARP enrichment commit) |
> | **2** — wizard renders dynamically (autofill from `vendor_data`) | ✅ | `f50df5a` |
> | **2** — per-field validation lifecycle + server-side routing | ✅ | `34d2b06` |
> | **5** — Kirigami `InlineError` widget + `Errors` global | ✅ | `a9d4b2a` |
> | **6** — smart Power button + connect-first | ✅ | `f18cd40` |
> | **4** — `PowerCyclePhase::DirectConnect` | ✅ | `f18cd40` (landed alongside Phase 6) |
> | **6.1** — LG `connect()` contract fix + scheme caching | ✅ | `20cb57f` |
> | **6.2** — daemon `power_on` probe heuristic (timeout + skip-when-off) | ⏳ queued | |
> | **4b** — `Pairing` / `Config` / `Connection` typed errors | partial (Config only); rest deferred | |
> | **4** — typed `Failed { reason }` payload | deferred — `Failed` still wire-format `(s)`; carries no typed reason yet | |
>
> Original plan body below — references to "draft" / "open
> questions" predate the resolved-question rounds; treat the
> `✅ resolved` markers + this status block as authoritative.

## Phase 6 follow-ups (integration-testing refinements)

These didn't appear in the original plan — they emerged from
journal analysis of a slow power-cycle (~45 s observed) after
Phase 6 landed on `feat/gui`. Both refinements are part of the
same in-flight branch as the rest of plan5; documented here so
the trail isn't lost when the work eventually merges.

### Phase 6.1 — LG `connect()` contract fix + scheme caching ✅

**Problem.** Phase 6's direct-connect probe was designed
assuming `TvConnector::connect()` fails fast on a single
attempt. The LG backend violated that: `LgConnector::connect`
wrapped a 5×retry loop with 1 s sleeps, each iteration calling
`Connection::connect` which itself probed `wss://` then fell
back to `ws://`. On a cold TV (just powered off, WS service
spinning down), each TLS-handshake-then-WS-close cycle cost
~5 s, and the outer loop burned all 25 s before giving up. The
daemon's probe never got the fast-fail it needed.

**Fix (commit `20cb57f`).**
- New typed `WebOsScheme { Wss, Ws }` enum on
  `LgVendorConfig.scheme`. The pairing handshake captures
  whichever scheme connected and persists it alongside
  `client_key`. Subsequent reconnects dispatch directly to
  that scheme via `WebOsClient::connect_with_scheme`, skipping
  the wss→ws fallback round-trip.
- Cross-vendor plumbing: `Credentials` gained a free-form
  `extra: HashMap<String, String>` for vendor-discovered
  metadata captured at pair time. Future Samsung / Sony
  backends can use the same slot for refresh URLs, capability
  strings, etc.
- Dropped the 5×retry-with-sleep loop in `LgConnector::connect`.
  It was duplicating `wake_and_connect`'s outer retry loop —
  now there's exactly one retry layer (in `wake_and_connect`).
- Documents the trait contract: **`TvConnector::connect()` is
  a single attempt; retry semantics belong in
  `wake_and_connect()`**.

**Result.** Power-on time on a cold-cycle dropped from ~45 s
to ~21 s. Each "TV not ready yet" connect attempt now fails
in <1 s instead of burning 5 s in fallback × retry; the WoL
loop probes aggressively without churning.

### Phase 6.2 — daemon `power_on` probe heuristic ⏳ queued

**Problem.** Even with the LG fix, the direct-connect probe
still costs ~5 s of TLS-handshake-timeout when the TV is
fully powered off — because the probe genuinely tries to
connect and the kernel waits for either a SYN/ACK or the
default timeout. On a "user just hit Power Off" sequence,
this is wasted budget: we *know* the TV is off.

**Fix (queued).** Two daemon-side adjustments to
`tv_manager::power_on`:

1. **Skip the probe when we know the TV is off.** Read
   `tv.powered_off` (already daemon-tracked) and the
   last-known cached `power_state`. If either says the TV is
   in standby / unknown, go straight to the WoL path —
   emit `WolSent → Connecting → Connected` without the
   `DirectConnect` phase. Saves ~5 s on the user-explicit
   power-cycle case.

2. **Time-bound the probe (~2 s) for the genuine
   "lost the connection" case.** Wrap `connect_tv` in a
   `tokio::time::timeout`. With Phase 6.1's fast-fail
   `connect()`, healthy reconnects complete in <500 ms; the
   timeout is belt-and-suspenders for misbehaving networks
   (firewall blackholes, etc.) where a TLS handshake hangs
   without RST.

Vendor-agnostic by design: both adjustments live on
`tv_manager`, no trait changes. The "what's a healthy probe
duration" is a daemon UX property, not a transport one.

**Estimated impact.** User-explicit power-cycle drops to
~16 s (subscription overhead + actual TV boot time only).
Reconnect-after-network-flap stays sub-second when the TV is
reachable, gives up in 2 s when it isn't — instead of taking
the full `CONNECT_MS` budget.

## Status (drafting v2 — historical, see Status block above for current)
> Round 2 incorporating user
> answers + external research (NetworkManager, ModemManager,
> Home Assistant config flows, KDE Kirigami inline messages,
> NN/g & Smashing Magazine on form-validation UX).
>
> This plan grew in scope from "fix the office-TV WoL bug" to
> "establish a clean architecture for vendor-specific
> configuration over D-Bus + a clean error vocabulary + a
> better-grounded UI for both." The bug fix is still the
> driving motivation; the rest falls out as the principled
> answers.
>
> Open questions are tagged **❓v2** for new ones in this
> revision. v1 questions that are now resolved are tagged
> **✅ resolved** with the answer inline.

## Bug recap (unchanged from v1)

Office TV paired via SSDP captured IP but not MAC →
`LgBackend::supports_wake()` returns `false` →
`tv_manager::power_on()` returns `WakeNotSupported` silently
(no log past `D-Bus: PowerOn`, no `Failed` phase emit, no
visible GUI error) → `powered_off=true` traps the TV until
daemon restart or manual config edit.

## Scope (revised)

| # | Item | Effort | Phase order |
|---|---|---|---|
| 1 | Vendor-specific config schema over D-Bus | medium-large | last (architecture-shaping) |
| 2 | Pairing wizard renders schema dynamically | medium | with 1 |
| 3 | MAC discovery via ARP + auto-fill | small | early |
| 4 | Daemon error vocabulary + phase accuracy | small | early |
| 5 | GUI inline error surface (Kirigami pattern) | small-medium | with 4 |
| 6 | Power-button UX for unreachable / no-wake | small | with 4-5 |
| 7 | "Open logs" affordance in About pane | small | last |
| 8 | Future: edit TV properties in detail pane | — | not in this plan |

## Design decisions (resolved from v1)

### Phase emit accuracy ✅ (was Q7)

User: *"don't send misleading signals … no backwards
compatibility requirements, refactor as needed."*

Resolution: add a `PowerCyclePhase::DirectConnect` variant
for the direct-connect-no-WoL path. The phase enum becomes:

| Phase | Meaning |
|---|---|
| `WolSent` | Magic packet on the wire |
| `DirectConnect` | Skipping WoL, going straight to TCP connect |
| `Connecting` | TCP / WS handshake in progress |
| `Connected` | Live + subscribed |
| `Failed` | Generic terminal failure (deprecated in favor of `Failed { reason }` carrying a typed error) |

Refactor consumers in tray + GUI accordingly.

### Toast position / convention ✅ (was Q4)

User: *"Following current conventions in KDE apps sounds
fine."*

Resolution: use **Kirigami inline-message** convention.
KDE's [inline messages](https://develop.kde.org/docs/getting-started/kirigami/components-inlinemessages/)
have four types (info / positive / warning / error), are
**sticky by default** (no auto-dismiss), have an optional
close button, can carry actions. Placed inline within the
content area (typically right under the pane header or right
above the offending control).

This swings my v1 recommendation: I had said "bottom-floating
toast, auto-dismiss" but that's a Material/web idiom, not a
KDE one. KDE's convention is more deliberate — the message
sticks until the user dismisses it or the underlying state
changes. For an error like "Power-on failed: no MAC
configured," sticky is correct (you want the user to see and
act).

### Toast scope ✅ (was Q5)

User: *"User-initiated errors only seems reasonable, but
perhaps we should also consider … direct users to journald
logs."*

Resolution: **user-initiated errors only** for the inline
message; auto-reconnect / background failures stay in
journald with an "Open logs" affordance in About (see Phase 7
below).

### Wizard policy for empty MAC ✅ (was Q2)

User: *"there must be a more generalizable approach … web
forms often have a 'this field is required' asterisk and a
lifecycle of validation."*

Resolution: build the wizard form on a **field-validation
lifecycle** modeled on the form-UX literature (NN/g,
Smashing Magazine). Per-field validation states:

| State | Trigger | UI |
|---|---|---|
| Pristine | Field never touched | Neutral |
| Editing | User typing | No premature error (avoid the "required" splash before the user has a chance to type) |
| Touched-empty | Lost focus while empty + required | `*` mark turns red; helper text becomes error: "MAC address required" |
| Invalid | Lost focus with non-matching value | Red border + helper text describes expected format ("Six hex pairs separated by colons or hyphens") |
| Valid | Lost focus with valid value | Green checkmark or just neutral border (positive validation per Baymard) |

Form `Next` button disables until all required fields are
valid. This is the same pattern Home Assistant's config_flow
uses, NetworkManager's nm-connection-editor uses, and
NN/g recommends.

This subsumes the simple "required asterisk" pattern and
generalizes to any vendor backend's required fields.

## Phase 4 — Daemon error vocabulary

### Why

Fixing only `WakeNotSupported` is myopic. Every place the
daemon currently returns a generic `Err` should map to a
typed error with a stable D-Bus error name. Following
[ModemManager's convention](https://www.freedesktop.org/software/ModemManager/doc/latest/ModemManager/ModemManager-Errors.html):

- A small **core** enum: `Failed`, `Cancelled`,
  `Unsupported`, `Unauthorized`, `InvalidArgs`,
  `InProgress`, `WrongState`, `NotFound`, `Retry`.
- Domain-specific enums layered on top, like
  `PowerError`, `PairingError`, `ConnectionError`,
  `ConfigError`. Each variant maps to:
  - A unique D-Bus error name
    (`org.tisvlay.Tv1.Power.NoMacConfigured`).
  - A short identifier (`power.no-mac`) for client-side
    routing.
  - A human-readable default message.

### Concrete error variants we can foresee

Distilled from the existing codebase + the user's research
prompt + similar projects (HA, NM, MM):

**Power errors**
- `NoMacConfigured` — "TV has no MAC address configured;
  WoL is unavailable."
- `WolSendFailed { source }` — "Couldn't send the
  Wake-on-LAN packet (network unreachable, …)"
- `WakeTimeout` — "Sent WoL but TV didn't come up within
  the wait window."
- `ConnectTimeout` — "TV reachable but not accepting our
  connection (firewall? wrong port?)"
- `AlreadyOn` / `AlreadyOff` — wrong-state recovery hints.

**Pairing errors**
- `Rejected` — TV said no.
- `Timeout` — User didn't accept the prompt in time.
- `WrongCredentials` — Stale client key.
- `BackendNotRegistered` — Vendor name doesn't map to a
  known backend.

**Config errors**
- `InvalidVendorConfig { field, reason }` — Schema
  validation failed (covers the MAC-format check, IP
  format, port range, etc.).
- `DuplicateTvId`.
- `InvalidProfileReference`.

**Connection errors**
- `Unreachable` — Carrier down / route missing.
- `AuthRejected` — TV revoked our key.

❓**v2 Q1**: This taxonomy mirrors ModemManager's grouping
(core + domain-specific). Would you prefer:
- **(a)** core + domain enums, each domain in its own file
  (`tisvlay-core/src/errors/power.rs`, `pairing.rs`,
  `config.rs`, `connection.rs`)
- **(b)** flat single enum `TisVlayError` with all variants
- I'd lean (a) — matches ModemManager, keeps each domain's
  vocabulary under the team that owns that subsystem (e.g.
  `tisvlay-lg` could even define vendor-specific subtypes
  later).

### Steps (Phase 4)

1. Define error enums + their D-Bus mapping in
   `tisvlay-core/src/errors/`.
2. Replace ad-hoc `anyhow::anyhow!()` and string errors in
   `tv_manager.rs`, `dbus/tv.rs`, vendor backends with the
   typed variants.
3. Each error converts to a D-Bus error via `From<…> for
   zbus::fdo::Error` (or a custom `DaemonError`-style
   wrapper).
4. `tv_manager::power_on` specifically: emit
   `PowerCyclePhase::Failed { reason: PowerError }` on the
   error path before returning, then `warn!` the typed
   error so journald has the structured fields.

## Phase 5 — GUI inline error surface

### Why

Mirror Kirigami's inline-message pattern in Slint. Each
pane gets a slot near the top for an `InlineError`
component. Rust pushes errors into a per-pane property when
a user-initiated action fails.

### Steps

1. New `InlineError` widget in `widgets.slint`:
   - 4 tones (info / positive / warning / error)
   - Icon (freedesktop `dialog-error` / `dialog-warning` /
     etc., resolved via the same theme lookup)
   - Title + message (rich text? probably plain)
   - Optional action button slot
   - Close (X) button on the right
   - Animates in (slide-down) when bound to non-empty
     content; stays sticky.
2. New `Errors` global with one slot per pane (`tvs:
   ErrorState`, `pairing: ErrorState`, etc.) where
   `ErrorState` is a struct `{ kind: string, message:
   string, action-label: string }`. Empty when no error.
3. Rust callback wiring: every `Daemon.power-on(...)`,
   `set_volume`, `set_managed`, etc. that returns an error
   pushes a typed message into the relevant `Errors.tvs`
   slot via `on_ui`.
4. The "Open logs" link (Phase 7) becomes a default action
   button on `Errors.kind == "internal"` errors.

❓**v2 Q2**: For errors that are **expected and
recoverable** (no MAC configured), the inline-message
should probably also link to the **fix**, not just describe
the error. e.g. error message: "Office has no MAC
configured. Wake-on-LAN won't work." + action button:
"Edit MAC". Ties Phase 5 to the future "edit TV properties"
work (Phase 8). Should we ship Phase 5 with edit-actions
stubbed out (button visible but disabled with "coming
soon"), or wait until edit-properties lands? I'd lean
**stub-then-fill**.

## Phase 6 — Power-button UX for unreachable / no-wake

### Why

User asked to think through three scenarios:

**(i) TV unreachable + no MAC** → Power-on can't possibly
work. Today: button enabled, click does nothing. Better:
button **disabled with tooltip** "Wake-on-LAN unavailable
— this TV has no MAC address configured. Pair the TV again
or edit the TV settings to add a MAC."

**(ii) TV reachable + connection lost** → Power-on
shouldn't WoL; should reconnect. Today: PowerOn() WoLs and
attempts connect. Better: PowerOn() probes connectivity
first; if reachable, skip WoL and emit `DirectConnect`
phase. If unreachable AND we have MAC, fall through to
WoL.

**(iii) "I want to verify TV is off without turning it
on"** → Currently no path. The power-state property
already exists on D-Bus; we could add a small **status
refresh** action that does a non-side-effect probe (TCP
SYN to the TV's port; if the connection establishes, the
TV is on; close immediately). Could be triggered manually
by a "Check status" button or automatically on pane focus.

❓**v2 Q3**: For (iii), separate "Check status" button OR
auto-probe on pane focus OR don't bother (let the existing
state-watcher loop pick it up)? My take: the existing
state-watcher already does periodic refresh; we don't need
a manual button unless the period is too long. Worth
checking the actual cadence first — what's the
`signal_debounce` / fallback poll interval today? If it's
≥ 30s, a "Check status" button is justified; if it's
~5s, the auto-loop is enough.

❓**v2 Q4**: For (ii), should `PowerOn` be the SAME button
that does the right thing depending on state, or split
into two buttons — `Power on` (forces WoL+connect) and
`Reconnect` (forces direct connect)? The Home Assistant
pattern leans toward "one button does the right thing
based on current state." My take: **one smart button**.
Rename if we add (iii) — i.e. the button could read "Wake
TV", "Reconnect", or "Power off" depending on
`(connected, powered_off, supports_wake)` triplet. Adds
clarity without adding click count.

### Steps (Phase 6)

1. `tv_manager::power_on` rewrites:
   - Probe direct connect first (small timeout, e.g. 1s)
   - On success: `DirectConnect` phase, register
     connection, emit `Connected`, done.
   - On probe failure + `supports_wake`: WoL path as
     today.
   - On probe failure + `!supports_wake`: emit `Failed {
     PowerError::NoMacConfigured }`, return.
2. GUI Power button:
   - Label: "Power on" / "Power off" / "Reconnect" based
     on triplet (depends on Q4 outcome).
   - `enabled: connected || supports_wake` — disabled when
     both off and no wake method.
   - Tooltip when disabled: explanation message.
3. `powered_off` flag semantics review: it should remain a
   *hint* for auto-reconnect ("user explicitly powered
   off, don't fight them") but not gate user-initiated
   actions. Cleared on successful PowerOn whether via WoL
   or direct.

## Phase 7 — "Open logs" affordance

### Why

Surface journald to advanced users / when something goes
sideways. User asked about a URI handler.

### Research result

There's no standard `journal://` URI scheme. Options:

- **(a)** Spawn `ksystemlog` directly via xdg-open. KSystemLog
  is the KDE journal viewer but has limited CLI filtering —
  it'd open with no filter, user has to filter manually.
- **(b)** Spawn a terminal with `journalctl -u tisvlayd -f`
  pre-typed. Requires picking a terminal (`xdg-terminal-exec`
  is a freedesktop spec but spotty).
- **(c)** Copy-to-clipboard a `journalctl` command with a
  toast saying "Open a terminal and paste."
- **(d)** Embed a small log viewer inside the GUI (read
  systemd journal via `systemd::journal::Journal::open` or
  shell out + parse).

Tradeoffs:
- (a) has best UX when KSystemLog is installed; degrades on
  GNOME / minimal installs.
- (b) is most accurate but UX is fragile.
- (c) is simplest and degradation-friendly.
- (d) is most polished but a sizable engineering chunk.

❓**v2 Q5**: Pick one. My recommendation: **(a) with (c) as
fallback** — try `xdg-open` on `kde-systemlog`-something /
the desktop file; on failure, fall back to copy-to-clipboard
with the journalctl command. Defer (d) until we have a real
need (debugging support stories).

❓**v2 Q6**: Should the "Open logs" button be in the About
pane (one-off place) or attached to inline-error messages
as the action button (every error gets a "View logs" jump)?
My take: **both**. About pane has the always-on shortcut;
errors with `kind == "internal"` get a contextual action.

## Phase 1 — MAC discovery via ARP

### Why (research-grounded)

Researched: SSDP responses don't include MAC. NetworkManager
populates MAC for ethernet/wifi via netlink (already a dep
in this workspace via `rtnetlink`). Same approach works for
us — after SSDP gives us an IP, do a netlink neighbor lookup
to fetch MAC.

### Steps

1. Add a `tisvlay-core::network` helper:
   `arp_lookup(ip: IpAddr) -> Option<MacAddr>` using
   `rtnetlink::NeighbourGetRequest`. Returns `None` when
   the neighbor table doesn't have an entry (cross-VLAN,
   wrong subnet) — caller falls back to "ask the user."
2. `LgDiscovery::discover_tvs` populates `mac` field on
   `DiscoveredTv` after each SSDP hit, by calling the
   helper. ARP lookup is fast (kernel-local) — no need for
   a parallelized fan-out.
3. Wire format change on Discovery's D-Bus return — see
   Phase 2 below for the right shape.

## Phase 2 — Pairing wizard renders config schema dynamically

### Why (research-grounded)

This is the big architectural piece. Looking at how peer
projects handle multi-vendor / multi-transport config:

**[NetworkManager](https://manpages.debian.org/testing/network-manager/nm-settings-dbus.5.en.html)**:
connection settings are a `a{sa{sv}}` dict-of-dicts. The
outer key is the connection type (`802-3-ethernet`,
`802-11-wireless`, `bluetooth`). Type-specific groups only
appear when the type is set. Clients (nmcli, GNOME
Settings) consult schema docs to know what fields apply per
type.

**[Home Assistant config flows](https://developers.home-assistant.io/docs/data_entry_flow_index/)**:
each integration declares a schema (voluptuous) describing
the form fields per step. The frontend renders the form
dynamically from the schema. Validation happens server-side;
errors come back keyed by field name with a translation key.

The TisVlay parallel: **the daemon should publish a config
schema per backend over D-Bus, and the wizard should render
form fields dynamically.** This solves not just the MAC
issue but every vendor field — Samsung's API key, Sony's
PSK, USB-CEC's serial port, etc.

### Schema shape

Borrowing from HA + NetworkManager:

```idl
struct ConfigField {
    name: string,       // "ip", "mac", "client_key", "tty"
    label: string,      // "IP address", "MAC address"
    helper: string,     // "Required for Wake-on-LAN."
    field_type: string, // "string" | "ip" | "mac" | "port" | "select" | "bool"
    required: bool,
    placeholder: string,
    default: string,
    autofill: string,   // "discovery.address", "discovery.mac" — name of a discovery field
    options: array<string>, // for "select" field-type
    validation: string, // optional regex; daemon also validates server-side
}

method GetVendorConfigSchema(vendor_id: string) -> Vec<ConfigField>
```

The wizard renders a `for field in schema: <FieldRenderer
field=... />` loop. `FieldRenderer` switches on
`field.field_type` to pick the right Slint widget
(LineEdit, ComboBox, Switch, etc.).

### Discovery wire format

Tied to schema: discovery results carry a
`vendor_data: a{ss}` map of free-form vendor-specific data
(the daemon's discovery code knows what fields the schema
declares; populates the matching ones). The wizard's
schema renderer auto-fills any field whose
`autofill: "discovery.<key>"` matches a populated entry.

This reframes v1's Q1 ("how to add MAC to discovery")
entirely — the answer is "add it as a `vendor_data` entry
under `mac`" and the rest is generic.

❓**v2 Q7**: This is a substantial refactor (D-Bus wire
break, schema definitions per backend, dynamic Slint form
rendering). Three options:

- **(a) Full schema-driven now** — implement Phase 2 in
  full, ~200-400 LoC across daemon + GUI, plus per-backend
  schema definitions. Sets up clean architecture before
  more vendors land.
- **(b) Hardcode + plan for refactor** — keep the wizard
  hardcoded for v1 (LG-specific fields), document that
  schema-driven rendering is the long-term direction, ship
  Phases 4-7 first to fix the bug. Refactor wizard later
  when adding Samsung/Sony.
- **(c) Hybrid** — define the schema types and the
  `GetVendorConfigSchema` D-Bus method now, but the wizard
  still uses hardcoded LG layout. Schema is consumed by
  validation logic only. Future PRs add dynamic rendering.

My take: **(b)**. The architectural value of (a) is real
but the bug is hurting the user *now*. (b) gets the bug
fixed in days, not weeks. Defer (a) to when we add a
second vendor and it'll motivate itself.

❓**v2 Q8**: If we go (b), what *minimum* changes happen to
the wizard now?
- Add MAC field (auto-filled from discovery's ARP lookup,
  editable, validated as MAC format)
- Mark fields as required-or-optional with red asterisks
- Per-field validation on blur (per the form-UX research)
- "Next" button disabled until required fields valid

That's ~50 LoC of Slint + ~30 LoC of Rust validation.
Compared to (a) it's a 4-5× smaller delta and addresses
the bug. Worth committing to (b) as the path forward?

## Phase 3 — MAC discovery via ARP

(See Phase 1 above. Numbered 1 in the order list, named
Phase 3 here for chronology with the rest of the plan
sequence.)

## Phase 8 — Edit TV properties (deferred / future)

User: *"I'd eventually like to enable users to edit TV
properties such as MAC address, IP address, and even Name
(that could then update the sanitized ID)…"*

This is its own can of worms (atomic config rewrites,
ID-renaming touching every cross-reference, GUI editing
chrome). Out of scope here. Filed in todo.md as a
follow-up.

## Suggested execution sequence (v3)

User picked **Q7 = (a) full schema-driven** + dropped
"Open logs" via Q5 + **localization plumbing from day one**
via v3 Q3.

0. **Phase 0 — i18n scaffold** (new):
   - Enable `slint = { features = ["gettext"] }` in
     `tisvlay-gui/Cargo.toml`.
   - Add `gettext-rs` for Rust-side dynamic strings.
   - Wrap all existing literal `.slint` strings with
     `@tr("...")` (mostly a sweep through panes / widgets).
   - Wrap Rust user-facing strings with `tr!` (errors,
     log messages that surface to UI — NOT internal
     debug logs).
   - Build pipeline: `slint-tr-extractor` invocation
     wired through `build.rs` or `just i18n-extract`,
     `lang/` directory layout, `init_translations!()` at
     startup.
   - Commit empty `<crate>.pot` (template) so future
     translators have a starting point. v1 ships only
     English (`lang/en/LC_MESSAGES/`).
   - No actual translations yet — but every string is
     ready to be translated without further refactoring.
1. **Phase 4** — typed errors (`tisvlay-core/src/errors/`),
   accurate phase emit (`PowerCyclePhase::DirectConnect`,
   typed `Failed { reason }`). Foundation; everything else
   uses these.
2. **Phase 1** — vendor config schema:
   - Schema types in `tisvlay-core` (`PairingField`,
     `FieldType`, `Validation`).
   - `TvConnector::pairing_schema()` trait method.
   - LG impl: declares fields (`ip`, `mac`, … with
     autofill keys).
   - `Manager1.GetPairingSchema(vendor) -> JSON-string`
     D-Bus method (per v3 Q1).
   - `Manager1.AddTv` gains schema-driven validation;
     returns `ConfigError::InvalidVendorConfig { field,
     reason }` on failure.
3. **Phase 3** — MAC discovery via ARP. Discovery wire
   format gains `vendor_data: a{ss}` map. LG discovery
   populates `vendor_data["mac"]` via `rtnetlink`
   neighbor lookup after each SSDP hit.
4. **Phase 2** — pairing wizard renders dynamically:
   - Vendor-combo selection triggers
     `GetPairingSchema(vendor)` fetch.
   - `for field in schema: FieldRenderer { field; ... }`
     loop renders.
   - Per-field validation lifecycle (pristine → editing →
     touched-empty → invalid → valid).
   - Auto-fill from `vendor_data` based on
     `field.autofill`.
5. **Phase 5** — Kirigami `InlineError` widget +
   `Errors` global with per-pane slots; user-initiated
   error paths in `register_callbacks` push typed
   messages.
6. **Phase 6** — smart Power button (label adapts;
   `enabled` reflects `(connected, powered_off,
   supports_wake)`); `tv_manager::power_on` rewritten:
   probe direct connect first, fall back to WoL,
   `Failed { NoMacConfigured }` when neither works.

Out of this plan:
- **Phase 7** (open logs) — dropped (deferred to future
  PR).
- **Phase 8** (edit TV properties) — deferred to its own
  plan.

## Open questions (consolidated)

**v2 (resolved)**:
- ✅ **Q1**: Per-domain error enums (ModemManager-style),
  files under `tisvlay-core/src/errors/` — vendor crates
  can later add their own subtypes (`tisvlay-lg/src/errors.rs`).
- ✅ **Q2**: Stub-then-fill — inline-error action buttons
  land with placeholder behavior; the actual "Edit MAC" /
  similar fillers come later.
- ✅ **Q3**: Investigated rather than answered. Default
  `fallback_poll_interval = 30s` (`dbus_client.rs:1364`)
  applies to GUI + tray. After Phase 6's smart Power
  button, the existing 30s poll + the explicit
  "ensure connected" click cover the user's scenarios —
  no separate "Check status" button.
- ✅ **Q4**: One smart button. Label adapts to
  `(connected, powered_off, supports_wake)`.
- ✅ **Q5**: Defer "Open logs" entirely to a future PR.
  Drop Phase 7 from this plan.
- ✅ **Q6**: N/A given Q5.
- ✅ **Q7**: Full schema-driven (option a). Project not
  yet released, no live users; pay the architectural cost
  now to avoid a future refactor.
- ✅ **Q8**: N/A given Q7.

**v3 (resolved)**:
- ✅ **Q1**: JSON-string over D-Bus.
  Investigated git history: `Manager1.AddTv` already takes
  `config_json: String` as of commit `7b7bf81`
  (Jan 2026 vendor-agnostic refactor), and the profile
  APIs return JSON strings too. So the JSON-on-D-Bus
  pattern is **established**, not a switch. Schema method
  fits the same idiom.
- ✅ **Q2**: Two methods. `GetPairingSchema(vendor)` now;
  `GetConfigSchema(vendor)` deferred to the edit-TV PR
  (Phase 8).
- ✅ **Q3**: **Localization plumbing scaffolded from day
  one**, even though only English ships at v1.
  - Slint side: enable `slint = { features = ["gettext"] }`,
    wrap literal strings in `.slint` files with `@tr("...")`,
    set up the `slint-tr-extractor` → `.pot` → `.po` →
    `.mo` build pipeline, `init_translations!` macro at
    startup, `lang/<locale>/LC_MESSAGES/<crate>.mo` layout
    convention (per [Slint docs](https://docs.slint.dev/latest/docs/slint/guide/development/translations/)).
  - Rust dynamic strings (errors, schema labels): use
    `gettext-rs` crate (Slint's own internals already use
    it; matches Linux ecosystem convention; user wanted
    to *learn* about localization → mainstream gettext is
    the right starting point).
  - Schema struct includes a
    `translation_key: Option<String>` field on each
    `PairingField` so the daemon can carry a key alongside
    the literal English label. v1 GUI uses the literal
    label; future PRs swap to a `gettext` lookup keyed by
    `translation_key` with the literal as fallback.
  - Vendor crates own their schema strings. `tisvlay-lg`
    will eventually ship `tisvlay-lg/lang/<locale>/...`.
- ✅ **Q4**: Field types vocabulary accepted: `string`,
  `password`, `ip`, `mac`, `port`, `int`, `bool`, `select`,
  `path`. Extend later as backends need it.
- ✅ **Q5**: Extend `TvConnector` trait.
  `fn pairing_schema(&self) -> Vec<PairingField>;`
- ✅ **Q6**: Empty schema → wizard skips the Configure
  step. No special-casing.

**v4 (resolved)**:
- ✅ **Q1**: Power button label stays "Power on" / "Power
  off" — established TisVlay vocabulary; smart logic
  picks WoL vs direct-connect path internally.
- ✅ **Q2**: Validation errors carry machine-readable
  `field: "mac"` (the schema's `name` identifier); GUI
  maps to the localized `label` for display. Daemon
  stays locale-agnostic.
- ✅ **Q3**: Commit `.pot` + `.po` to git; generate `.mo`
  at build time via `slint_build` / `build.rs`. Standard
  gettext convention.

**v4 (original — preserved for context)**:

The schema-driven approach forces a few design choices.

- **v4 Q1**: Power button label naming for the
  "disconnected, can wake" state — "Power on" or "Wake TV"?
  HA uses "Turn on" generically; KDE Connect uses "Power
  on." The user has been using "Power on/off" consistently.
  My pick: keep "Power on" — it's the established TisVlay
  vocabulary, and "Wake TV" implies physical-act (remote)
  whereas "Power on" implies logical-act (state). For the
  reconnect-no-WoL case ("connected=false, but TV likely
  reachable"): same button, since the smart logic figures
  out which path to take. Confirm?

- **v4 Q2**: Validation error mapping — when the daemon
  rejects pairing input because field `mac` is invalid,
  the error carries `field: "mac"` (the schema's `name`
  identifier, machine-readable). The GUI maps `"mac"` back
  to the schema field's localized `label` for display.
  Confirm? (Alternative: daemon sends the localized label
  directly, but then a different-locale GUI couldn't
  re-localize.)

- **v4 Q3**: Build pipeline integration — adding
  `slint-tr-extractor` to the build means a new build
  step (extract POT, merge with existing POs). Should we
  add a `just i18n-extract` recipe + commit the resulting
  `.pot`/`.po`/`.mo` files to the repo, or generate them
  at build time only? My pick: commit `.pot` (template,
  source-of-truth for translators) and `.po` (per-locale
  translations); generate `.mo` at build time from `.po`
  via `slint_build` (or a small `build.rs` step). Standard
  gettext convention.
- ✅ Wire-format option for MAC field — moot under schema
  approach (Phase 2 reframes); resolved as "vendor_data
  map" if we go (a) or (c), or just an extra field on the
  current tuple if we go (b).
- ✅ Empty-MAC policy — built into the form-validation
  lifecycle; required fields driven by schema (or
  hardcoded under (b)).
- ✅ `WakeNotSupported` variant — yes, distinct (per Phase
  4 vocabulary).
- ✅ Toast position — Kirigami inline-message, sticky.
- ✅ Toast scope — user-initiated only.
- ✅ Recovery approach — connect-first PowerOn (Phase 6),
  with edit-MAC deferred to Phase 8.
- ✅ Phase emit accuracy — refactor; add `DirectConnect`,
  drop bare `Failed` for typed `Failed { reason }`.

## References

- [NetworkManager nm-settings-dbus(5)](https://manpages.debian.org/testing/network-manager/nm-settings-dbus.5.en.html)
  — dict-of-dicts settings, type-keyed groups
- [Home Assistant Data Entry Flow](https://developers.home-assistant.io/docs/data_entry_flow_index/)
  — per-step schema, server-side validation, frontend
  renders dynamically
- [ModemManager Errors reference](https://www.freedesktop.org/software/ModemManager/doc/latest/ModemManager/ModemManager-Errors.html)
  — core + domain enums, stable D-Bus error names
- [Kirigami inline messages](https://develop.kde.org/docs/getting-started/kirigami/components-inlinemessages/)
  — sticky, 4 types, optional close, optional actions
- [Smashing Magazine: Live validation UX](https://www.smashingmagazine.com/2022/09/inline-validation-web-forms-ux/)
  — avoid premature validation, validate on blur
- [NN/g: Marking Required Fields](https://www.nngroup.com/articles/required-fields/)
  — red asterisk convention
