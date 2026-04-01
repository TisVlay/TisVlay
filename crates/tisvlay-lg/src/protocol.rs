//! LG WebOS SSAP protocol message types.
//!
//! The SSAP (Simple Service Access Protocol) is a JSON-based protocol
//! used by LG WebOS TVs over WebSocket connections.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outgoing message to the TV.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    /// Unique request ID for correlating responses.
    pub id: String,
    /// Message type: "hello", "register", or "request".
    #[serde(rename = "type")]
    pub msg_type: RequestType,
    /// SSAP endpoint URI (e.g., "ssap://system/turnOff").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Request payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

/// Request message types.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestType {
    /// Registration/pairing request.
    Register,
    /// Command request.
    Request,
    /// Subscribe to state changes.
    Subscribe,
}

/// Incoming message from the TV.
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    /// Request ID this response corresponds to.
    pub id: Option<String>,
    /// Response type.
    #[serde(rename = "type")]
    pub msg_type: ResponseType,
    /// SSAP endpoint URI (present in subscription updates).
    #[serde(default)]
    pub uri: Option<String>,
    /// Response payload.
    #[serde(default)]
    pub payload: Value,
    /// Error message (for error responses).
    pub error: Option<String>,
}

/// Response message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseType {
    /// Response to a request.
    Response,
    /// Registration was successful.
    Registered,
    /// Error response.
    Error,
    /// Hello response with TV info.
    Hello,
}

/// Registration payload sent during pairing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterPayload {
    /// Whether to force a new pairing prompt.
    pub force_pairing: bool,
    /// Pairing type (usually "PROMPT").
    pub pairing_type: String,
    /// Application manifest with requested permissions.
    pub manifest: Manifest,
    /// Previously obtained client key (None for new pairing).
    #[serde(rename = "client-key", skip_serializing_if = "Option::is_none")]
    pub client_key: Option<String>,
}

/// Application manifest for registration.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Requested permissions.
    pub permissions: Vec<String>,
    /// Manifest signatures (required by TV).
    pub signatures: Vec<Signature>,
    /// Manifest version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<i32>,
    /// Application signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    /// Whether this is a signed manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed: Option<SignedInfo>,
}

/// Signature information.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    /// Base64-encoded signature.
    pub signature: String,
    /// Signature version.
    pub signature_version: i32,
}

/// Signed manifest info for elevated permissions.
///
/// This section contains permissions that require a signed manifest to be
/// granted by the TV. The signature, serial, and app identity must match
/// the standard LG WebOS client credentials (shared by bscpylgtv, aiowebostv,
/// LGTVCompanion, etc.).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedInfo {
    /// App ID (must be "com.lge.test" to match the standard signed manifest).
    pub app_id: String,
    /// Vendor ID.
    pub vendor_id: String,
    /// Creation timestamp.
    pub created: String,
    /// Elevated permissions granted via the signed manifest.
    pub permissions: Vec<String>,
    /// Serial number matching the standard signed manifest.
    pub serial: String,
}

impl RegisterPayload {
    /// Create a new registration payload.
    ///
    /// If `client_key` is provided, attempts to reconnect with existing pairing.
    /// If `None`, a new pairing prompt will be shown on the TV.
    pub fn new(client_key: Option<String>) -> Self {
        Self {
            force_pairing: false,
            pairing_type: "PROMPT".to_string(),
            manifest: Manifest::default(),
            client_key,
        }
    }
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            permissions: vec![
                // Power control
                "CONTROL_POWER".to_string(),
                "READ_POWER_STATE".to_string(),
                // Display control
                "CONTROL_DISPLAY".to_string(),
                // Input control
                "CONTROL_INPUT_TV".to_string(),
                "CONTROL_INPUT_MEDIA_PLAYBACK".to_string(),
                // Audio control
                "CONTROL_AUDIO".to_string(),
                // App control
                "LAUNCH".to_string(),
                "LAUNCH_WEBAPP".to_string(),
                "CLOSE".to_string(),
                // Read state
                "READ_APP_STATUS".to_string(),
                "READ_INSTALLED_APPS".to_string(),
                "READ_INPUT_DEVICE_LIST".to_string(),
                "READ_NETWORK_STATE".to_string(),
                "READ_TV_CHANNEL_LIST".to_string(),
                "READ_RUNNING_APPS".to_string(),
                "READ_UPDATE_INFO".to_string(),
                "READ_CURRENT_CHANNEL".to_string(),
                // Mouse/keyboard input
                "CONTROL_INPUT_JOYSTICK".to_string(),
                "CONTROL_MOUSE_AND_KEYBOARD".to_string(),
                // Notifications
                "READ_NOTIFICATIONS".to_string(),
                // Settings (needed for backlight/picture settings read)
                "READ_SETTINGS".to_string(),
                "CONTROL_TV_SCREEN".to_string(),
                "WRITE_NOTIFICATION_TOAST".to_string(),
            ],
            signatures: vec![Signature {
                // Standard signature used by most WebOS clients
                signature: "eyJhbGdvcml0aG0iOiJSU0EtU0hBMjU2Iiwia2V5SWQiOiJ0ZXN0LXNp\
                    Z25pbmctY2VydCIsInNpZ25hdHVyZVZlcnNpb24iOjF9.hrVRgjCwXVvE2OOSpDZ\
                    58hR+59aFNwYDyjQgKk3auukd7pcegmE2CzPCa0bJ0ZsRAcKkCTJrWo5iDzNhMBWR\
                    yaMOv5zWSrthlf7G128qvIlpMT0YNY+n/FaOHE73uLrS/g7swl3/qH/BGFG2Hu4RlL\
                    48eb3lLKqTt2xKHdCs6Cd4RMfJPYnzgvI4BNrFUKsjkcu+WD4OO2A27Pq1n50cMchmcaXadJhGrOqH5\
                    LAcDNKfbu0ntmGnjK4XeUq5Duzp+6hGzNZL5g0ByaJ1gBWNqjnfU+d0DYPzQ8kFmGvS+Zf\
                    q+SB6FBVhJ+OjP2vBJlObaUT9zLHw8GdjN8iCq+iH/A=="
                    .replace(['\n', ' '], ""),
                signature_version: 1,
            }],
            manifest_version: Some(1),
            app_version: Some("1.1".to_string()),
            // Signed manifest section — provides elevated permissions for
            // settings access (WRITE_SETTINGS, WRITE_NOTIFICATION_ALERT).
            // Matches the manifest used by bscpylgtv and other LG WebOS clients.
            signed: Some(SignedInfo {
                app_id: "com.lge.test".to_string(),
                created: "20140509".to_string(),
                vendor_id: "com.lge".to_string(),
                permissions: vec![
                    "TEST_SECURE".to_string(),
                    "CONTROL_INPUT_TEXT".to_string(),
                    "CONTROL_MOUSE_AND_KEYBOARD".to_string(),
                    "READ_INSTALLED_APPS".to_string(),
                    "READ_LGE_SDX".to_string(),
                    "READ_NOTIFICATIONS".to_string(),
                    "SEARCH".to_string(),
                    "WRITE_SETTINGS".to_string(),
                    "WRITE_NOTIFICATION_ALERT".to_string(),
                    "CONTROL_POWER".to_string(),
                    "READ_CURRENT_CHANNEL".to_string(),
                    "READ_RUNNING_APPS".to_string(),
                    "READ_UPDATE_INFO".to_string(),
                    "UPDATE_FROM_REMOTE_APP".to_string(),
                    "READ_LGE_TV_INPUT_EVENTS".to_string(),
                    "READ_TV_CURRENT_TIME".to_string(),
                ],
                serial: "2f930e2d2cfe083771f68e4fe7bb07".to_string(),
            }),
        }
    }
}

/// Registered response payload containing the client key.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RegisteredPayload {
    /// Client key for future connections.
    pub client_key: String,
}

/// Power state response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerState {
    /// Current power state.
    pub state: String,
    /// Processing state (for wake-up).
    #[serde(default)]
    pub processing: Option<String>,
}

/// Volume response wrapper (TV returns volume inside volumeStatus).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeResponse {
    /// Volume status (may be nested or at top level).
    #[serde(default)]
    pub volume_status: Option<VolumeInfo>,
    /// Direct volume (some responses put it at top level).
    #[serde(default)]
    pub volume: Option<i32>,
    /// Direct muted (some responses put it at top level).
    #[serde(default)]
    pub muted: Option<bool>,
}

impl VolumeResponse {
    /// Extract volume info, handling both nested and flat response formats.
    pub fn into_volume_info(self) -> Option<VolumeInfo> {
        if let Some(status) = self.volume_status {
            Some(status)
        } else if let Some(volume) = self.volume {
            Some(VolumeInfo {
                volume,
                muted: self.muted.unwrap_or(false),
            })
        } else {
            None
        }
    }
}

/// Volume info.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    /// Current volume level (0-100).
    pub volume: i32,
    /// Whether audio is muted.
    #[serde(default)]
    pub muted: bool,
}

/// External input info.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalInput {
    /// Input ID (e.g., "HDMI_1").
    pub id: String,
    /// Display label.
    pub label: String,
    /// Whether this input is currently connected.
    #[serde(default)]
    pub connected: bool,
    /// Input type.
    #[serde(default)]
    pub app_id: Option<String>,
}

/// List of external inputs.
#[derive(Debug, Clone, Deserialize)]
pub struct ExternalInputList {
    /// Available inputs.
    pub devices: Vec<ExternalInput>,
}

impl Request {
    /// Create a register request.
    pub fn register(id: impl Into<String>, payload: RegisterPayload) -> Self {
        Self {
            id: id.into(),
            msg_type: RequestType::Register,
            uri: None,
            payload: Some(serde_json::to_value(payload).unwrap()),
        }
    }

    /// Create a command request.
    pub fn command(id: impl Into<String>, uri: impl Into<String>, payload: Value) -> Self {
        Self {
            id: id.into(),
            msg_type: RequestType::Request,
            uri: Some(uri.into()),
            payload: Some(payload),
        }
    }

    /// Create a subscribe request.
    pub fn subscribe(id: impl Into<String>, uri: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            msg_type: RequestType::Subscribe,
            uri: Some(uri.into()),
            payload: None,
        }
    }
}
