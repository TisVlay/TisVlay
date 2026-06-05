//! Pure ESC/VP21 protocol codec.
//!
//! This module owns the wire grammar of Epson's ESC/VP21 control
//! vocabulary as it travels over the raw TCP transport: the
//! `ESC/VP.net` handshake bytes, command framing, reply parsing, and
//! the value-table mappings (power codes, input-source codes). It is
//! deliberately I/O-free — every function takes bytes or strings and
//! returns parsed values, so the whole grammar is table-testable
//! without a socket. The TCP plumbing lives in
//! [`transport`](crate::transport); the high-level command sequencing
//! lives in [`client`](crate::client).

use tisvlay_core::PowerState;

/// Handshake greeting sent to open an ESC/VP.net session.
///
/// The ASCII `ESC/VP.net` (10 bytes) followed by the 6-byte trailer
/// `0x10 0x03 0x00 0x00 0x00 0x00`. The projector answers with a
/// 16-byte frame validated by [`handshake_ok`].
pub const HELLO: &[u8] = b"ESC/VP.net\x10\x03\x00\x00\x00\x00";

/// Length of both the handshake greeting and its reply.
pub const HANDSHAKE_LEN: usize = 16;

/// Magic prefix the projector echoes in a successful handshake reply.
const HANDSHAKE_NAME: &[u8] = b"ESC/VP.net";

/// Status byte (offset 14) the projector sets to ASCII space (`0x20`)
/// to signal the handshake succeeded.
const HANDSHAKE_OK_BYTE: u8 = 0x20;

/// Whether a 16-byte handshake reply indicates a good connection.
///
/// The projector echoes `ESC/VP.net` in the first 10 bytes and sets
/// byte 14 to ASCII space (`0x20`) on success. Anything shorter, or
/// with a mismatched prefix or status byte, is a failed handshake.
pub fn handshake_ok(resp: &[u8]) -> bool {
    resp.len() >= HANDSHAKE_LEN
        && &resp[0..HANDSHAKE_NAME.len()] == HANDSHAKE_NAME
        && resp[14] == HANDSHAKE_OK_BYTE
}

/// Frame a command for the wire by appending a carriage return.
///
/// A query is `NAME?` (e.g. `"PWR?"`); a set is the full command
/// (e.g. `"PWR ON"`, `"SOURCE 30"`). Both get a trailing `\r`.
pub fn frame(cmd: &str) -> String {
    format!("{cmd}\r")
}

/// Build the query command body for `NAME` (`NAME?`), without the
/// carriage return — [`frame`] appends the terminator when the body is
/// sent. Pass the result to the transport's `command`.
pub fn query_body(name: &str) -> String {
    format!("{name}?")
}

/// A decoded ESC/VP21 reply.
///
/// The projector answers a query with `NAME=VALUE` and a set command
/// with a bare `:` acknowledgement. `ERR` signals a command error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// A query reply value — the part after `=` in `NAME=VALUE`.
    Value(String),
    /// A bare acknowledgement (`:`) returned for a successful set.
    Ack,
    /// The projector returned `ERR`.
    Error,
}

/// Parse a raw reply frame into a [`Reply`].
///
/// Decodes the bytes as UTF-8 (lossily — the protocol is ASCII), then
/// strips trailing carriage returns, colons, and NUL padding that the
/// 16-byte fixed read leaves behind. The cleaned token is then
/// classified:
///
/// - `ERR` → [`Reply::Error`].
/// - empty (a bare `:` ack stripped to nothing) → [`Reply::Ack`].
/// - `NAME=VALUE` → [`Reply::Value`] of the part after `=`.
/// - any other bare token → [`Reply::Value`] of the token (defensive;
///   some firmware answers a query with a bare value).
pub fn parse_reply(raw: &[u8]) -> Reply {
    let decoded = String::from_utf8_lossy(raw);
    // Strip the framing the projector pads a fixed-size read with:
    // trailing CR, the `:` ack terminator, and NUL fill bytes.
    let cleaned = decoded.trim_end_matches(['\r', ':', '\0']);
    let cleaned = cleaned.trim();

    if cleaned == "ERR" {
        return Reply::Error;
    }
    if cleaned.is_empty() {
        return Reply::Ack;
    }
    match cleaned.split_once('=') {
        Some((_name, value)) => Reply::Value(value.trim().to_string()),
        None => Reply::Value(cleaned.to_string()),
    }
}

/// Internal classification of an ESC/VP21 `PWR?` status code.
///
/// The projector's `PWR?` query returns a 2-character status code.
/// `On` and the two standby/off codes are unambiguous; the `Busy`
/// transitional code is *not* always directionally distinct on every
/// firmware, so the client disambiguates it against the last power
/// command it issued (see [`crate::client`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerCode {
    /// `01` — the light source is on and an image is being projected.
    On,
    /// `02` — warming up (a directionally-distinct transitional code,
    /// when the firmware provides it).
    Warming,
    /// `03` — cooling down (a directionally-distinct transitional
    /// code, when the firmware provides it).
    Cooling,
    /// A standby / powered-off code (`00`, `04`, `05`).
    Standby,
    /// An unrecognized status code.
    Unknown,
}

/// Classify a raw `PWR?` status code string.
///
/// Known codes:
/// - `01` → [`PowerCode::On`]
/// - `02` → [`PowerCode::Warming`]
/// - `03` → [`PowerCode::Cooling`]
/// - `00` / `04` / `05` → [`PowerCode::Standby`]
/// - anything else → [`PowerCode::Unknown`]
///
/// Whether `02` and `03` are *actually* distinct on the target
/// hardware is unverified — the client tracks last-power-intent to
/// disambiguate a generic busy code regardless. Parsing is
/// case-insensitive and tolerant of surrounding whitespace.
pub fn parse_power_code(code: &str) -> PowerCode {
    match code.trim().to_ascii_uppercase().as_str() {
        "01" => PowerCode::On,
        "02" => PowerCode::Warming,
        "03" => PowerCode::Cooling,
        "00" | "04" | "05" => PowerCode::Standby,
        _ => PowerCode::Unknown,
    }
}

/// The direction a power transition is heading, tracked by the client
/// from the last `PWR ON` / `PWR OFF` it sent.
///
/// Used to disambiguate a generic busy `PWR?` code into a warming or
/// cooling [`PowerState`] when the firmware doesn't give a
/// directionally-distinct code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PowerIntent {
    /// No power command issued yet this session.
    #[default]
    None,
    /// A `PWR ON` was the last power command sent.
    PoweringOn,
    /// A `PWR OFF` was the last power command sent.
    PoweringOff,
}

/// Resolve a `PWR?` status code plus the last power-intent into a
/// [`PowerState`].
///
/// The directionally-distinct codes win when present: `02`→
/// [`PowerState::WarmingUp`], `03`→[`PowerState::CoolingDown`]. A
/// generic busy reading that the firmware reports as `Warming`/
/// `Cooling` is already resolved by [`parse_power_code`]; the intent
/// fallback only matters when a firmware reports the *same* busy code
/// for both directions — modeled here by mapping
/// [`PowerCode::Unknown`] (the "busy but indistinct" reading) through
/// the intent.
///
/// `On` maps to [`PowerState::Active`] here; the compound
/// screen-off derivation (on + A/V-muted → [`PowerState::ScreenOff`])
/// is applied a layer up where the `MUTE?` reading is also available.
pub fn power_state_from(code: PowerCode, intent: PowerIntent) -> PowerState {
    match code {
        PowerCode::On => PowerState::Active,
        PowerCode::Warming => PowerState::WarmingUp,
        PowerCode::Cooling => PowerState::CoolingDown,
        PowerCode::Standby => PowerState::Standby,
        PowerCode::Unknown => match intent {
            PowerIntent::PoweringOn => PowerState::WarmingUp,
            PowerIntent::PoweringOff => PowerState::CoolingDown,
            PowerIntent::None => PowerState::Unknown,
        },
    }
}

/// Parse a `MUTE?` reply value into an A/V-mute boolean.
///
/// `ON` (or `01`, defensively) means the image is blanked (A/V mute
/// engaged — the laser-saving "screen off"). `OFF` (or `00`) means the
/// image is projected. Parsing is case-insensitive; an unrecognized
/// value returns `None`.
pub fn parse_mute(value: &str) -> Option<bool> {
    match value.trim().to_ascii_uppercase().as_str() {
        "ON" | "01" => Some(true),
        "OFF" | "00" => Some(false),
        _ => None,
    }
}

/// Map an ESC/VP21 `SOURCE?` hex code to a normalized input ID.
///
/// The daemon treats input IDs as opaque strings; the normalized set
/// is `HDMI_1` / `HDMI_2` / `PC` / `VIDEO` / `USB` / `LAN`. Codes
/// `40` and `41` both map to `VIDEO`. Matching is case-insensitive on
/// the hex code. Returns `None` for an unrecognized code.
pub fn source_code_to_input(code: &str) -> Option<&'static str> {
    match code.trim().to_ascii_uppercase().as_str() {
        "30" => Some("HDMI_1"),
        "A0" => Some("HDMI_2"),
        "10" => Some("PC"),
        "40" | "41" => Some("VIDEO"),
        "52" => Some("USB"),
        "53" => Some("LAN"),
        _ => None,
    }
}

/// Map a normalized input ID back to its canonical `SOURCE` hex code.
///
/// The inverse of [`source_code_to_input`], used to build the
/// `SOURCE <hex>` set command. `VIDEO` round-trips to `40` (the
/// canonical of the two video codes). Matching is case-insensitive on
/// the input ID. Returns `None` for an unknown input ID.
pub fn input_to_source_code(input: &str) -> Option<&'static str> {
    match input.trim().to_ascii_uppercase().as_str() {
        "HDMI_1" => Some("30"),
        "HDMI_2" => Some("A0"),
        "PC" => Some("10"),
        "VIDEO" => Some("40"),
        "USB" => Some("52"),
        "LAN" => Some("53"),
        _ => None,
    }
}

/// The full set of input IDs the Epson backend exposes, with display
/// labels. `connected` is reported `false` — ESC/VP21 has no per-input
/// connection probe, so the list advertises capability, not live link
/// status.
pub fn known_inputs() -> Vec<(&'static str, &'static str)> {
    vec![
        ("HDMI_1", "HDMI 1"),
        ("HDMI_2", "HDMI 2"),
        ("PC", "PC"),
        ("VIDEO", "Video"),
        ("USB", "USB"),
        ("LAN", "LAN"),
    ]
}

/// ESC/VP21 command names used by the backend.
pub mod cmd {
    /// Power command/query name (`PWR`).
    pub const POWER: &str = "PWR";
    /// A/V-mute command/query name (`MUTE`).
    pub const MUTE: &str = "MUTE";
    /// Input-source command/query name (`SOURCE`).
    pub const SOURCE: &str = "SOURCE";
    /// Laser-brightness command/query name (`LUMLEVEL`).
    pub const LUMLEVEL: &str = "LUMLEVEL";

    /// `PWR ON` set command body.
    pub const POWER_ON: &str = "PWR ON";
    /// `PWR OFF` set command body.
    pub const POWER_OFF: &str = "PWR OFF";
    /// `MUTE ON` set command body (engage A/V mute / blank the image).
    pub const MUTE_ON: &str = "MUTE ON";
    /// `MUTE OFF` set command body (disengage A/V mute / project the
    /// image).
    pub const MUTE_OFF: &str = "MUTE OFF";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_is_sixteen_bytes() {
        assert_eq!(HELLO.len(), HANDSHAKE_LEN);
        assert_eq!(&HELLO[0..10], b"ESC/VP.net");
        assert_eq!(&HELLO[10..], &[0x10, 0x03, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn handshake_ok_accepts_valid_reply() {
        let mut resp = [0u8; 16];
        resp[0..10].copy_from_slice(b"ESC/VP.net");
        resp[14] = 0x20;
        assert!(handshake_ok(&resp));
    }

    #[test]
    fn handshake_rejects_wrong_status_byte() {
        let mut resp = [0u8; 16];
        resp[0..10].copy_from_slice(b"ESC/VP.net");
        resp[14] = 0x00;
        assert!(!handshake_ok(&resp));
    }

    #[test]
    fn handshake_rejects_wrong_prefix() {
        let mut resp = [0u8; 16];
        resp[0..10].copy_from_slice(b"NOT/VP.net");
        resp[14] = 0x20;
        assert!(!handshake_ok(&resp));
    }

    #[test]
    fn handshake_rejects_short_reply() {
        assert!(!handshake_ok(b"ESC/VP.net"));
        assert!(!handshake_ok(&[]));
    }

    #[test]
    fn frame_appends_cr() {
        assert_eq!(frame("PWR ON"), "PWR ON\r");
        assert_eq!(frame("SOURCE 30"), "SOURCE 30\r");
        // A query body framed for the wire round-trips to `NAME?\r`.
        assert_eq!(query_body("PWR"), "PWR?");
        assert_eq!(frame(&query_body("PWR")), "PWR?\r");
        assert_eq!(frame(&query_body("LUMLEVEL")), "LUMLEVEL?\r");
    }

    #[test]
    fn parse_reply_query_value() {
        assert_eq!(parse_reply(b"PWR=01\r:"), Reply::Value("01".into()));
        assert_eq!(parse_reply(b"SOURCE=30\r:"), Reply::Value("30".into()));
        assert_eq!(parse_reply(b"MUTE=ON\r:"), Reply::Value("ON".into()));
        assert_eq!(parse_reply(b"LUMLEVEL=50\r:"), Reply::Value("50".into()));
    }

    #[test]
    fn parse_reply_value_tolerates_nul_padding() {
        // A fixed 16-byte read leaves NUL fill after the payload.
        let mut raw = b"PWR=02\r:".to_vec();
        raw.resize(16, 0);
        assert_eq!(parse_reply(&raw), Reply::Value("02".into()));
    }

    #[test]
    fn parse_reply_set_ack() {
        // A successful set is a bare `:` ack that strips to empty.
        assert_eq!(parse_reply(b":"), Reply::Ack);
        assert_eq!(parse_reply(b"\r:"), Reply::Ack);
        let mut padded = b":".to_vec();
        padded.resize(16, 0);
        assert_eq!(parse_reply(&padded), Reply::Ack);
    }

    #[test]
    fn parse_reply_error() {
        assert_eq!(parse_reply(b"ERR\r:"), Reply::Error);
        assert_eq!(parse_reply(b"ERR"), Reply::Error);
    }

    #[test]
    fn parse_reply_bare_value_without_equals() {
        // Defensive: some firmware may answer a query with a bare value.
        assert_eq!(parse_reply(b"01\r:"), Reply::Value("01".into()));
    }

    #[test]
    fn power_codes_classify() {
        assert_eq!(parse_power_code("01"), PowerCode::On);
        assert_eq!(parse_power_code("02"), PowerCode::Warming);
        assert_eq!(parse_power_code("03"), PowerCode::Cooling);
        assert_eq!(parse_power_code("00"), PowerCode::Standby);
        assert_eq!(parse_power_code("04"), PowerCode::Standby);
        assert_eq!(parse_power_code("05"), PowerCode::Standby);
        assert_eq!(parse_power_code("99"), PowerCode::Unknown);
        // Whitespace + case tolerance.
        assert_eq!(parse_power_code(" 01 "), PowerCode::On);
    }

    #[test]
    fn power_state_distinct_codes_win() {
        assert_eq!(
            power_state_from(PowerCode::On, PowerIntent::None),
            PowerState::Active
        );
        assert_eq!(
            power_state_from(PowerCode::Warming, PowerIntent::PoweringOff),
            PowerState::WarmingUp,
            "a distinct warming code beats a stale cooling intent",
        );
        assert_eq!(
            power_state_from(PowerCode::Cooling, PowerIntent::PoweringOn),
            PowerState::CoolingDown,
            "a distinct cooling code beats a stale warming intent",
        );
        assert_eq!(
            power_state_from(PowerCode::Standby, PowerIntent::None),
            PowerState::Standby
        );
    }

    #[test]
    fn power_state_indistinct_busy_uses_intent() {
        // A firmware that returns the same indistinct busy code for both
        // directions (modeled as PowerCode::Unknown) is disambiguated by
        // the last power command issued.
        assert_eq!(
            power_state_from(PowerCode::Unknown, PowerIntent::PoweringOn),
            PowerState::WarmingUp,
        );
        assert_eq!(
            power_state_from(PowerCode::Unknown, PowerIntent::PoweringOff),
            PowerState::CoolingDown,
        );
        assert_eq!(
            power_state_from(PowerCode::Unknown, PowerIntent::None),
            PowerState::Unknown,
        );
    }

    #[test]
    fn mute_parses_both_forms() {
        assert_eq!(parse_mute("ON"), Some(true));
        assert_eq!(parse_mute("OFF"), Some(false));
        assert_eq!(parse_mute("01"), Some(true));
        assert_eq!(parse_mute("00"), Some(false));
        assert_eq!(parse_mute("on"), Some(true));
        assert_eq!(parse_mute("garbage"), None);
    }

    #[test]
    fn source_codes_round_trip() {
        for (code, input) in [
            ("30", "HDMI_1"),
            ("A0", "HDMI_2"),
            ("10", "PC"),
            ("52", "USB"),
            ("53", "LAN"),
        ] {
            assert_eq!(source_code_to_input(code), Some(input));
            assert_eq!(input_to_source_code(input), Some(code));
        }
    }

    #[test]
    fn source_video_codes_both_map_to_video() {
        assert_eq!(source_code_to_input("40"), Some("VIDEO"));
        assert_eq!(source_code_to_input("41"), Some("VIDEO"));
        // VIDEO round-trips to the canonical 40.
        assert_eq!(input_to_source_code("VIDEO"), Some("40"));
    }

    #[test]
    fn source_unknown_code_is_none() {
        assert_eq!(source_code_to_input("FF"), None);
        assert_eq!(input_to_source_code("HDMI_9"), None);
    }

    #[test]
    fn source_matching_is_case_insensitive() {
        assert_eq!(source_code_to_input("a0"), Some("HDMI_2"));
        assert_eq!(input_to_source_code("hdmi_1"), Some("30"));
    }

    #[test]
    fn known_inputs_cover_the_normalized_set() {
        let ids: Vec<&str> = known_inputs().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["HDMI_1", "HDMI_2", "PC", "VIDEO", "USB", "LAN"]);
    }
}
