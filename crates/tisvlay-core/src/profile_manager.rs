//! Backend-agnostic mutation surface for the per-profile D-Bus
//! interfaces.
//!
//! Implemented by tisvlayd's `TvManager` (system profiles) and
//! tisvlay-session's `SessionProfileManager` (user profiles) so the
//! per-profile interface wrappers (`SystemProfileService`,
//! `UserProfileService`) can call into either side via the same
//! method names. Pairs with the [`profile_field_props!`] macro to
//! collapse the per-property getter/setter pairs into a single
//! invocation per interface.
//!
//! Read paths stay direct (each manager exposes its own typed
//! accessors); create / duplicate / delete are likewise direct on the
//! concrete manager — they aren't dispatched by the per-profile
//! interfaces, so they don't need to be in the trait.
//!
//! [`profile_field_props!`]: crate::profile_field_props

use async_trait::async_trait;
use uuid::Uuid;

/// Error variants the per-profile interface impls need to distinguish
/// when mapping to `zbus::fdo::Error`. Each [`ProfileManager`] impl
/// maps from its native error type into one of these variants; the
/// interface wrapper's `profile_err` helper then maps to the
/// appropriate D-Bus error name (`Invalid` → `InvalidArgs`, `Failed`
/// → `Failed`, `AccessDenied` → `AccessDenied`).
#[derive(Debug)]
pub enum ProfileError {
    /// Caller-supplied input was invalid (enum parse failure,
    /// reserved name, empty name, duplicate name, unknown field).
    Invalid(String),
    /// Operation failed for state-machine reasons (unknown UUID,
    /// persistence I/O error).
    Failed(String),
    /// Operation rejected for authorization reasons (e.g. deleting
    /// the reserved-default profile, deleting a referenced profile).
    AccessDenied(String),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Failed(m) | Self::AccessDenied(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ProfileError {}

/// Mutation surface for the per-profile D-Bus interfaces, abstracted
/// over which manager owns the underlying state.
///
/// The trait is intentionally narrow: it covers only the four
/// mutation paths the `Properties.Set` / `ResetField` / `ResetAll`
/// wire methods land on. Both implementations follow the same
/// internal pattern — validate input, mutate state under a write
/// lock, refresh dependent caches, persist to disk, emit a
/// body-changed event — but the trait doesn't prescribe the
/// internals.
#[async_trait]
pub trait ProfileManager: Send + Sync + 'static {
    /// Set one string-typed field by wire name (snake_case). Used
    /// for the typed enum properties and `Name`.
    async fn set_string_field(
        &self,
        uuid: Uuid,
        field: &str,
        value: &str,
    ) -> Result<(), ProfileError>;

    /// Set one u32-typed numeric field by wire name. Today only the
    /// session side's `idle_timeout_seconds` is numeric; system side
    /// returns [`ProfileError::Invalid`] so adding the first numeric
    /// system field is a one-arm extension rather than a trait
    /// change.
    async fn set_u32_field(&self, uuid: Uuid, field: &str, value: u32) -> Result<(), ProfileError>;

    /// Reset one field back to inheriting (clear `Some(_)` →
    /// `None` on the body).
    async fn reset_field(&self, uuid: Uuid, field: &str) -> Result<(), ProfileError>;

    /// Reset every field back to inheriting. Name is preserved.
    async fn reset_all(&self, uuid: Uuid) -> Result<(), ProfileError>;
}
