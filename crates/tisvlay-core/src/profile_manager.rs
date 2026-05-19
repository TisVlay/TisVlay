//! Backend-agnostic mutation surface for the per-profile D-Bus
//! interfaces. Implemented by `TvManager` (system profiles) and
//! `SessionProfileManager` (user profiles) so the per-profile
//! interface wrappers can call into either side via the same method
//! names. Read paths stay direct; create / duplicate / delete are
//! direct on the concrete manager since the per-profile interfaces
//! don't dispatch them.

use async_trait::async_trait;
use uuid::Uuid;

/// Error variants the per-profile interface impls need to distinguish
/// when mapping to `zbus::fdo::Error` (`Invalid` → `InvalidArgs`,
/// `Failed` → `Failed`, `AccessDenied` → `AccessDenied`).
#[derive(Debug)]
pub enum ProfileError {
    Invalid(String),
    Failed(String),
    AccessDenied(String),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Invalid(m) | Self::Failed(m) | Self::AccessDenied(m)) = self;
        f.write_str(m)
    }
}

impl std::error::Error for ProfileError {}

/// Mutation surface for the per-profile D-Bus interfaces, abstracted
/// over which manager owns the underlying state. Covers only the four
/// paths `Properties.Set` / `ResetField` / `ResetAll` dispatch.
#[async_trait]
pub trait ProfileManager: Send + Sync + 'static {
    /// Set a string-typed field by wire name (snake_case). Used for
    /// the typed enum properties and `Name`.
    async fn set_string_field(
        &self,
        uuid: Uuid,
        field: &str,
        value: &str,
    ) -> Result<(), ProfileError>;

    /// Set a `u32`-typed numeric field. Today only the session side's
    /// `idle_timeout_seconds` is numeric; system side returns
    /// `Invalid`.
    async fn set_u32_field(&self, uuid: Uuid, field: &str, value: u32) -> Result<(), ProfileError>;

    /// Reset one field back to inheriting (clear `Some(_)` → `None`).
    async fn reset_field(&self, uuid: Uuid, field: &str) -> Result<(), ProfileError>;

    /// Reset every field back to inheriting. Name is preserved.
    async fn reset_all(&self, uuid: Uuid) -> Result<(), ProfileError>;
}
