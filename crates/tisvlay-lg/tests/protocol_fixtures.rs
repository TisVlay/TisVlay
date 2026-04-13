//! Fixture-based tests for SSAP protocol message parsing.
//!
//! Each fixture is a JSON file captured (or reconstructed) from real
//! LG TV interactions. Tests verify that `Response` deserializes
//! correctly and that domain-specific payload types extract the
//! expected values.

use rstest::rstest;
use tisvlay_lg::protocol::{
    ExternalInputList, PowerState, RegisterPayload, RegisteredPayload, Request, Response,
    ResponseType, VolumeResponse,
};

// ---------------------------------------------------------------------------
// Response deserialization from fixture JSON
// ---------------------------------------------------------------------------

#[rstest]
#[case::register_phase1(
    include_str!("fixtures/register_response_phase1.json"),
    "register_0",
    ResponseType::Response
)]
#[case::register_phase2(
    include_str!("fixtures/register_response_phase2.json"),
    "register_0",
    ResponseType::Registered
)]
#[case::power_active(
    include_str!("fixtures/power_state_active.json"),
    "status_1",
    ResponseType::Response
)]
#[case::power_screen_off(
    include_str!("fixtures/power_state_screen_off.json"),
    "status_2",
    ResponseType::Response
)]
#[case::volume_nested(
    include_str!("fixtures/volume_nested.json"),
    "status_3",
    ResponseType::Response
)]
#[case::volume_flat(
    include_str!("fixtures/volume_flat.json"),
    "status_4",
    ResponseType::Response
)]
#[case::input_list(
    include_str!("fixtures/external_input_list.json"),
    "status_5",
    ResponseType::Response
)]
#[case::foreground_app(
    include_str!("fixtures/foreground_app.json"),
    "status_6",
    ResponseType::Response
)]
#[case::backlight(
    include_str!("fixtures/backlight_settings.json"),
    "status_7",
    ResponseType::Response
)]
#[case::error(
    include_str!("fixtures/error_response.json"),
    "cmd_1",
    ResponseType::Error
)]
#[case::command_ok(
    include_str!("fixtures/command_success.json"),
    "cmd_2",
    ResponseType::Response
)]
fn parse_response_fixture(
    #[case] json: &str,
    #[case] expected_id: &str,
    #[case] expected_type: ResponseType,
) {
    let response: Response = serde_json::from_str(json).expect("fixture should parse as Response");
    assert_eq!(response.id.as_deref(), Some(expected_id));
    assert_eq!(response.msg_type, expected_type);
}

// ---------------------------------------------------------------------------
// Domain-specific payload extraction
// ---------------------------------------------------------------------------

#[test]
fn registered_payload_extracts_client_key() {
    let json = include_str!("fixtures/register_response_phase2.json");
    let response: Response = serde_json::from_str(json).unwrap();
    let payload: RegisteredPayload =
        serde_json::from_value(response.payload).expect("should parse as RegisteredPayload");
    assert_eq!(payload.client_key, "abc123def456");
}

#[rstest]
#[case::active("fixtures/power_state_active.json", "Active", Some("None"))]
#[case::screen_off("fixtures/power_state_screen_off.json", "Screen Off", None)]
fn power_state_extraction(
    #[case] fixture: &str,
    #[case] expected_state: &str,
    #[case] expected_processing: Option<&str>,
) {
    // include_str! requires a literal, so match on the fixture path.
    let json = match fixture {
        "fixtures/power_state_active.json" => {
            include_str!("fixtures/power_state_active.json")
        }
        "fixtures/power_state_screen_off.json" => {
            include_str!("fixtures/power_state_screen_off.json")
        }
        _ => unreachable!(),
    };
    let response: Response = serde_json::from_str(json).unwrap();
    let power: PowerState = serde_json::from_value(response.payload).unwrap();
    assert_eq!(power.state, expected_state);
    assert_eq!(power.processing.as_deref(), expected_processing);
}

#[test]
fn volume_nested_format() {
    let json = include_str!("fixtures/volume_nested.json");
    let response: Response = serde_json::from_str(json).unwrap();
    let vol: VolumeResponse = serde_json::from_value(response.payload).unwrap();
    let info = vol.into_volume_info().expect("should extract volume info");
    assert_eq!(info.volume, 25);
    assert!(!info.muted);
}

#[test]
fn volume_flat_format() {
    let json = include_str!("fixtures/volume_flat.json");
    let response: Response = serde_json::from_str(json).unwrap();
    let vol: VolumeResponse = serde_json::from_value(response.payload).unwrap();
    let info = vol.into_volume_info().expect("should extract volume info");
    assert_eq!(info.volume, 42);
    assert!(info.muted);
}

#[test]
fn external_input_list_parsing() {
    let json = include_str!("fixtures/external_input_list.json");
    let response: Response = serde_json::from_str(json).unwrap();
    let list: ExternalInputList = serde_json::from_value(response.payload).unwrap();
    assert_eq!(list.devices.len(), 4);
    assert_eq!(list.devices[0].id, "HDMI_1");
    assert!(list.devices[0].connected);
    assert_eq!(list.devices[1].id, "HDMI_2");
    assert!(!list.devices[1].connected);
}

#[test]
fn error_response_has_error_message() {
    let json = include_str!("fixtures/error_response.json");
    let response: Response = serde_json::from_str(json).unwrap();
    assert_eq!(response.msg_type, ResponseType::Error);
    assert_eq!(
        response.error.as_deref(),
        Some("401 insufficient permissions")
    );
}

// ---------------------------------------------------------------------------
// Request serialization round-trips
// ---------------------------------------------------------------------------

#[test]
fn register_request_serializes() {
    let payload = RegisterPayload::new(Some("test-key".to_string()));
    let request = Request::register("reg_0", payload);
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["id"], "reg_0");
    assert_eq!(json["type"], "register");
    assert_eq!(json["payload"]["client-key"], "test-key");
    assert_eq!(json["payload"]["pairingType"], "PROMPT");
    // Manifest should have permissions and signatures
    assert!(json["payload"]["manifest"]["permissions"].is_array());
    assert!(json["payload"]["manifest"]["signatures"].is_array());
}

#[test]
fn register_request_without_key_omits_field() {
    let payload = RegisterPayload::new(None);
    let request = Request::register("reg_1", payload);
    let json = serde_json::to_value(&request).unwrap();
    assert!(
        json["payload"]["client-key"].is_null(),
        "client-key should be absent when None"
    );
}

#[test]
fn command_request_serializes() {
    let request = Request::command(
        "cmd_1",
        "ssap://audio/setVolume",
        serde_json::json!({"volume": 30}),
    );
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["id"], "cmd_1");
    assert_eq!(json["type"], "request");
    assert_eq!(json["uri"], "ssap://audio/setVolume");
    assert_eq!(json["payload"]["volume"], 30);
}

#[test]
fn subscribe_request_serializes() {
    let request = Request::subscribe("sub_1", "ssap://audio/getVolume");
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["id"], "sub_1");
    assert_eq!(json["type"], "subscribe");
    assert_eq!(json["uri"], "ssap://audio/getVolume");
    // No payload for plain subscribe
    assert!(json.get("payload").is_none() || json["payload"].is_null());
}

#[test]
fn subscribe_with_payload_serializes() {
    let request = Request::subscribe_with_payload(
        "sub_2",
        "ssap://settings/getSystemSettings",
        serde_json::json!({"category": "picture", "keys": ["backlight"]}),
    );
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["type"], "subscribe");
    assert_eq!(json["payload"]["category"], "picture");
}

// ---------------------------------------------------------------------------
// Manifest defaults
// ---------------------------------------------------------------------------

#[test]
fn default_manifest_has_expected_permissions() {
    let payload = RegisterPayload::new(None);
    // Spot-check key permissions
    assert!(payload
        .manifest
        .permissions
        .contains(&"CONTROL_POWER".to_string()));
    assert!(payload
        .manifest
        .permissions
        .contains(&"CONTROL_AUDIO".to_string()));
    assert!(payload
        .manifest
        .permissions
        .contains(&"READ_SETTINGS".to_string()));
    // Signed section should exist
    let signed = payload
        .manifest
        .signed
        .as_ref()
        .expect("signed section should exist");
    assert_eq!(signed.app_id, "com.lge.test");
    assert!(signed.permissions.contains(&"WRITE_SETTINGS".to_string()));
}
