//! Backend registry for managing TV vendor backends.
//!
//! This module re-exports the registry from `tisvlay-backends` crate,
//! which is the single composition point for all vendor implementations.
//!
//! The daemon should NOT import vendor crates directly (e.g., `tisvlay-lg`).
//! All vendor access goes through the registry from `tisvlay-backends`.

// Re-export from tisvlay-backends
pub use tisvlay_backends::{BackendRegistry, create_registry};
