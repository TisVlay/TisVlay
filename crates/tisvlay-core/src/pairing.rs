//! Pairing state machine for TV authentication.
//!
//! Different TV vendors have different pairing mechanisms:
//! - **LG WebOS**: Interactive prompt on TV screen, user must accept
//! - **Other vendors**: May require PIN entry, password, OAuth, etc.
//!
//! This module provides a unified abstraction for these diverse flows via
//! the [`PairingSession`] type and [`PairingState`] enum.
//!
//! # Architecture
//!
//! The pairing flow is modeled as a state machine:
//!
//! ```text
//! [Started] ──► [AwaitingTvPrompt] ──► [Complete]
//!                     │                    ▲
//!                     ▼                    │
//!              [AwaitingUserInput] ────────┘
//!                     │
//!                     ▼
//!                 [Failed]
//! ```
//!
//! GUI/CLI clients subscribe to state changes via [`PairingSession::subscribe`]
//! and respond to input requests via [`PairingSession::submit_input`].

use serde::{Deserialize, Serialize};
use std::fmt;
use tokio::sync::{mpsc, watch};

use crate::Secret;

/// Type of user input required during pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    /// No input required, just waiting (e.g., for user to accept prompt on TV).
    None,
    /// Enter a PIN code displayed on the TV.
    Pin,
    /// Enter a pre-configured password.
    Password,
}

impl fmt::Display for InputType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Pin => write!(f, "pin"),
            Self::Password => write!(f, "password"),
        }
    }
}

impl InputType {
    /// Parse from a D-Bus string.
    pub fn from_dbus_string(s: &str) -> Self {
        match s {
            "pin" => Self::Pin,
            "password" => Self::Password,
            _ => Self::None,
        }
    }

    /// Convert to a D-Bus string.
    pub fn to_dbus_string(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pin => "pin",
            Self::Password => "password",
        }
    }
}

/// Credentials obtained from successful pairing.
///
/// `data` is a [`Secret`]; `Debug` is written by hand so the `extra` values
/// (future refresh tokens) stay out of it too, since `Debug` output can
/// reach logs through `?value` fields and `#[instrument]`.
#[derive(Clone)]
pub struct Credentials {
    /// Vendor-specific credential data.
    ///
    /// For LG WebOS, this is the client_key.
    /// For other vendors, this might be a token, password hash, etc.
    pub data: Secret,

    /// Optional credential type identifier.
    pub credential_type: Option<String>,

    /// Vendor-discovered metadata captured during pairing, free-form
    /// `key → value`. Lets backends persist *what they learned at
    /// pair time* alongside the primary credential without polluting
    /// the daemon's vendor-agnostic relay.
    ///
    /// Examples:
    /// - LG: `extra["scheme"] = "wss"` — which WebSocket scheme the
    ///   TV accepted, so subsequent reconnects skip the wss→ws
    ///   fallback.
    /// - Future: Samsung PSK refresh URLs, OAuth refresh tokens,
    ///   etc.
    ///
    /// Read by `TvConnector::build_config_from_pairing` to populate
    /// the persisted vendor config. Other vendors leave it empty.
    pub extra: std::collections::HashMap<String, String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut extra_keys: Vec<&str> = self.extra.keys().map(String::as_str).collect();
        extra_keys.sort_unstable();
        f.debug_struct("Credentials")
            .field("data", &self.data)
            .field("credential_type", &self.credential_type)
            .field("extra_keys", &extra_keys)
            .finish()
    }
}

impl Credentials {
    /// Create new credentials with just a data string.
    pub fn new(data: impl Into<Secret>) -> Self {
        Self {
            data: data.into(),
            credential_type: None,
            extra: std::collections::HashMap::new(),
        }
    }

    /// Create new credentials with a type identifier.
    pub fn with_type(data: impl Into<Secret>, credential_type: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            credential_type: Some(credential_type.into()),
            extra: std::collections::HashMap::new(),
        }
    }

    /// Insert a key/value into [`Self::extra`] (builder shorthand).
    #[must_use]
    pub fn with_extra(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra.insert(key.into(), value.into());
        self
    }
}

/// Error that occurred during pairing.
#[derive(Debug, Clone)]
pub struct PairingError {
    /// Error code for programmatic handling.
    pub code: PairingErrorCode,
    /// Human-readable error message.
    pub message: String,
}

impl PairingError {
    /// Create a new pairing error.
    pub fn new(code: PairingErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// User rejected the pairing prompt on the TV.
    pub fn rejected() -> Self {
        Self::new(PairingErrorCode::Rejected, "Pairing was rejected on the TV")
    }

    /// Pairing timed out waiting for user response.
    pub fn timeout() -> Self {
        Self::new(
            PairingErrorCode::Timeout,
            "Pairing timed out - no response from TV or user",
        )
    }

    /// Connection to TV failed.
    pub fn connection_failed(msg: impl Into<String>) -> Self {
        Self::new(PairingErrorCode::ConnectionFailed, msg)
    }

    /// Invalid input provided by user.
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::new(PairingErrorCode::InvalidInput, msg)
    }

    /// Pairing was cancelled.
    pub fn cancelled() -> Self {
        Self::new(PairingErrorCode::Cancelled, "Pairing was cancelled")
    }
}

impl fmt::Display for PairingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PairingError {}

/// Error codes for pairing failures.
///
/// The wire/routing form is [`Self::as_str`] (namespaced `pairing.<kebab>`),
/// carried as the pairing-progress payload's `code` field and matched
/// by clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingErrorCode {
    /// User rejected pairing on the TV.
    Rejected,
    /// Pairing timed out.
    Timeout,
    /// Could not connect to TV.
    ConnectionFailed,
    /// Invalid PIN/password provided.
    InvalidInput,
    /// Pairing was cancelled by user.
    Cancelled,
    /// Unknown/internal error.
    Unknown,
}

impl PairingErrorCode {
    /// Stable machine-readable identifier for routing on clients.
    /// Format mirrors the `<domain>.<kebab-case-variant>` convention
    /// shared with the other typed-error domains (`power.*`, `config.*`,
    /// `device.*`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rejected => "pairing.rejected",
            Self::Timeout => "pairing.timeout",
            Self::ConnectionFailed => "pairing.connection-failed",
            Self::InvalidInput => "pairing.invalid-input",
            Self::Cancelled => "pairing.cancelled",
            Self::Unknown => "pairing.unknown",
        }
    }
}

impl fmt::Display for PairingErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Current state of the pairing process.
#[derive(Debug, Clone)]
pub enum PairingState {
    /// Waiting for user to respond to prompt on TV screen.
    ///
    /// For LG WebOS, this means "Please accept the pairing request on your TV".
    AwaitingTvPrompt {
        /// Human-readable message to display.
        message: String,
    },

    /// Waiting for user to provide input (PIN, password, etc.).
    AwaitingUserInput {
        /// Human-readable prompt to display.
        prompt: String,
        /// Type of input required.
        input_type: InputType,
    },

    /// Pairing completed successfully.
    Complete {
        /// Credentials obtained from pairing.
        credentials: Credentials,
    },

    /// Pairing failed.
    Failed {
        /// Error that caused the failure.
        error: PairingError,
    },
}

impl PairingState {
    /// Create a new "awaiting TV prompt" state.
    pub fn awaiting_tv_prompt(message: impl Into<String>) -> Self {
        Self::AwaitingTvPrompt {
            message: message.into(),
        }
    }

    /// Create a new "awaiting user input" state.
    pub fn awaiting_user_input(prompt: impl Into<String>, input_type: InputType) -> Self {
        Self::AwaitingUserInput {
            prompt: prompt.into(),
            input_type,
        }
    }

    /// Create a "complete" state with credentials.
    pub fn complete(credentials: Credentials) -> Self {
        Self::Complete { credentials }
    }

    /// Create a "failed" state with an error.
    pub fn failed(error: PairingError) -> Self {
        Self::Failed { error }
    }

    /// Get the D-Bus state string.
    pub fn to_dbus_state(&self) -> &'static str {
        match self {
            Self::AwaitingTvPrompt { .. } => "awaiting_tv",
            Self::AwaitingUserInput { .. } => "awaiting_input",
            Self::Complete { .. } => "complete",
            Self::Failed { .. } => "failed",
        }
    }

    /// Get the human-readable message for this state.
    pub fn message(&self) -> &str {
        match self {
            Self::AwaitingTvPrompt { message } => message,
            Self::AwaitingUserInput { prompt, .. } => prompt,
            Self::Complete { .. } => "Pairing complete",
            Self::Failed { error } => &error.message,
        }
    }

    /// Get the input type for this state.
    pub fn input_type(&self) -> InputType {
        match self {
            Self::AwaitingUserInput { input_type, .. } => *input_type,
            _ => InputType::None,
        }
    }

    /// Check if pairing is complete (successfully or with failure).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete { .. } | Self::Failed { .. })
    }

    /// Check if pairing completed successfully.
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete { .. })
    }

    /// Check if pairing failed.
    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// Get credentials if pairing completed successfully.
    pub fn credentials(&self) -> Option<&Credentials> {
        match self {
            Self::Complete { credentials } => Some(credentials),
            _ => None,
        }
    }

    /// Get error if pairing failed.
    pub fn error(&self) -> Option<&PairingError> {
        match self {
            Self::Failed { error } => Some(error),
            _ => None,
        }
    }
}

impl Default for PairingState {
    fn default() -> Self {
        Self::AwaitingTvPrompt {
            message: "Waiting for TV response...".to_string(),
        }
    }
}

/// Commands that can be sent to a pairing session.
#[derive(Debug)]
pub enum PairingCommand {
    /// Submit user input (PIN, password, etc.).
    SubmitInput(String),
    /// Cancel the pairing process.
    Cancel,
}

/// A pairing session that tracks the state of a pairing attempt.
///
/// This provides a unified interface for diverse pairing mechanisms across
/// different TV vendors.
///
/// # Usage
///
/// ```ignore
/// let session = backend.start_pairing("192.168.1.100");
///
/// // Subscribe to state changes
/// let mut state_rx = session.subscribe();
///
/// // Monitor state in a separate task
/// tokio::spawn(async move {
///     while state_rx.changed().await.is_ok() {
///         let state = state_rx.borrow();
///         match &*state {
///             PairingState::AwaitingTvPrompt { message } => {
///                 println!("Please accept on TV: {}", message);
///             }
///             PairingState::AwaitingUserInput { prompt, input_type } => {
///                 println!("Enter {}: {}", input_type, prompt);
///             }
///             PairingState::Complete { credentials } => {
///                 println!("Pairing complete! Key: {:?}", credentials.data);
///                 break;
///             }
///             PairingState::Failed { error } => {
///                 println!("Pairing failed: {}", error);
///                 break;
///             }
///         }
///     }
/// });
///
/// // If PIN is requested, submit it
/// session.submit_input("1234").await?;
///
/// // Or cancel if needed
/// session.cancel().await?;
/// ```
#[derive(Debug)]
pub struct PairingSession {
    /// Channel for sending commands to the backend.
    command_tx: mpsc::Sender<PairingCommand>,
    /// Channel for receiving state updates.
    state_rx: watch::Receiver<PairingState>,
}

impl PairingSession {
    /// Create a new pairing session.
    ///
    /// This is typically called by backend implementations, not by users.
    /// Use [`TvConnector::start_pairing`](crate::TvConnector::start_pairing) instead.
    pub fn new(
        command_tx: mpsc::Sender<PairingCommand>,
        state_rx: watch::Receiver<PairingState>,
    ) -> Self {
        Self {
            command_tx,
            state_rx,
        }
    }

    /// Create a pairing session with its channel endpoints.
    ///
    /// Returns the session and the backend-side channel endpoints.
    /// The backend should:
    /// - Listen on `command_rx` for user input
    /// - Send state updates via `state_tx`
    pub fn create() -> (
        Self,
        mpsc::Receiver<PairingCommand>,
        watch::Sender<PairingState>,
    ) {
        let (command_tx, command_rx) = mpsc::channel(8);
        let (state_tx, state_rx) = watch::channel(PairingState::default());

        let session = Self::new(command_tx, state_rx);
        (session, command_rx, state_tx)
    }

    /// Subscribe to state changes.
    ///
    /// Returns a receiver that will be notified when the pairing state changes.
    pub fn subscribe(&self) -> watch::Receiver<PairingState> {
        self.state_rx.clone()
    }

    /// Clone the session's command sender.
    ///
    /// Allows callers outside the session's owning scope to send
    /// `PairingCommand::Cancel` (and future commands) without holding
    /// the session itself. The daemon's world keeps this sender on
    /// the pairing entity so the `cancel-pair` rpc can reach
    /// in-flight pair and re-pair handshakes.
    pub fn command_sender(&self) -> mpsc::Sender<PairingCommand> {
        self.command_tx.clone()
    }

    /// Get the current pairing state.
    pub fn state(&self) -> PairingState {
        self.state_rx.borrow().clone()
    }

    /// Submit user input (PIN, password, etc.).
    ///
    /// This should be called when the state is [`PairingState::AwaitingUserInput`].
    pub async fn submit_input(&self, input: impl Into<String>) -> Result<(), PairingError> {
        self.command_tx
            .send(PairingCommand::SubmitInput(input.into()))
            .await
            .map_err(|_| PairingError::new(PairingErrorCode::Unknown, "Pairing session closed"))
    }

    /// Cancel the pairing process.
    pub async fn cancel(&self) -> Result<(), PairingError> {
        self.command_tx
            .send(PairingCommand::Cancel)
            .await
            .map_err(|_| PairingError::new(PairingErrorCode::Unknown, "Pairing session closed"))
    }

    /// Wait for the pairing to complete (success or failure).
    ///
    /// Returns the final credentials on success, or an error on failure.
    pub async fn wait_for_completion(&mut self) -> Result<Credentials, PairingError> {
        loop {
            // Check current state first
            let state = self.state_rx.borrow().clone();
            match state {
                PairingState::Complete { credentials } => return Ok(credentials),
                PairingState::Failed { error } => return Err(error),
                _ => {}
            }

            // Wait for next change
            if self.state_rx.changed().await.is_err() {
                return Err(PairingError::new(
                    PairingErrorCode::Unknown,
                    "Pairing session closed unexpectedly",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_type_display() {
        assert_eq!(InputType::None.to_string(), "none");
        assert_eq!(InputType::Pin.to_string(), "pin");
        assert_eq!(InputType::Password.to_string(), "password");
    }

    #[test]
    fn pairing_error_code_as_str_is_namespaced() {
        assert_eq!(PairingErrorCode::Rejected.as_str(), "pairing.rejected");
        assert_eq!(
            PairingErrorCode::ConnectionFailed.as_str(),
            "pairing.connection-failed",
        );
        assert_eq!(PairingErrorCode::Unknown.as_str(), "pairing.unknown");
        // Display delegates to as_str.
        assert_eq!(
            PairingErrorCode::Timeout.to_string(),
            PairingErrorCode::Timeout.as_str(),
        );
    }

    #[test]
    fn test_input_type_dbus() {
        assert_eq!(InputType::from_dbus_string("pin"), InputType::Pin);
        assert_eq!(InputType::from_dbus_string("password"), InputType::Password);
        assert_eq!(InputType::from_dbus_string("unknown"), InputType::None);

        assert_eq!(InputType::Pin.to_dbus_string(), "pin");
    }

    #[test]
    fn test_pairing_state_dbus() {
        let state = PairingState::awaiting_tv_prompt("Accept on TV");
        assert_eq!(state.to_dbus_state(), "awaiting_tv");
        assert_eq!(state.message(), "Accept on TV");

        let state = PairingState::awaiting_user_input("Enter PIN", InputType::Pin);
        assert_eq!(state.to_dbus_state(), "awaiting_input");
        assert_eq!(state.input_type(), InputType::Pin);

        let state = PairingState::complete(Credentials::new("key123"));
        assert_eq!(state.to_dbus_state(), "complete");
        assert!(state.is_complete());

        let state = PairingState::failed(PairingError::rejected());
        assert_eq!(state.to_dbus_state(), "failed");
        assert!(state.is_failed());
    }

    #[test]
    fn test_credentials() {
        let creds = Credentials::new("mykey");
        assert_eq!(creds.data.expose(), "mykey");
        assert!(creds.credential_type.is_none());

        let creds = Credentials::with_type("mykey", "client_key");
        assert_eq!(creds.data.expose(), "mykey");
        assert_eq!(creds.credential_type, Some("client_key".to_string()));
    }

    #[test]
    fn test_pairing_error_display() {
        let err = PairingError::rejected();
        assert_eq!(err.to_string(), "Pairing was rejected on the TV");

        let err = PairingError::timeout();
        assert!(err.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_pairing_session() {
        let (session, mut command_rx, state_tx) = PairingSession::create();

        // Initial state
        assert!(matches!(
            session.state(),
            PairingState::AwaitingTvPrompt { .. }
        ));

        // Update state to awaiting input
        state_tx
            .send(PairingState::awaiting_user_input(
                "Enter PIN",
                InputType::Pin,
            ))
            .unwrap();

        // Submit input
        session.submit_input("1234").await.unwrap();

        // Verify command received
        let cmd = command_rx.recv().await.unwrap();
        assert!(matches!(cmd, PairingCommand::SubmitInput(s) if s == "1234"));

        // Complete pairing
        state_tx
            .send(PairingState::complete(Credentials::new("key123")))
            .unwrap();

        let state = session.state();
        assert!(state.is_complete());
        assert_eq!(state.credentials().unwrap().data.expose(), "key123");
    }

    /// Credential data and extra values are secrets; `Debug` output (which can
    /// reach logs) shows only that they exist.
    #[test]
    fn credentials_debug_redacts_secrets() {
        let creds = Credentials {
            data: "secret-client-key-1234".into(),
            credential_type: Some("client_key".into()),
            extra: [(
                "refresh_token".to_string(),
                "secret-refresh-5678".to_string(),
            )]
            .into(),
        };
        for shown in [format!("{creds:?}"), format!("{creds:#?}")] {
            assert!(!shown.contains("secret-client-key-1234"), "{shown}");
            assert!(!shown.contains("secret-refresh-5678"), "{shown}");
            assert!(shown.contains("[REDACTED]"), "{shown}");
            assert!(
                shown.contains("refresh_token"),
                "extra keys stay visible: {shown}"
            );
        }
    }
}
