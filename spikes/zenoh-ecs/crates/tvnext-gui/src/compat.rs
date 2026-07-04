//! Fork seam: one namespace merging the transport-free core types with
//! the zenoh client SDK, so the root GUI's `use crate::compat::{…}`
//! lists port with a single path swap. New code should import from the
//! real crates directly; this module exists to keep the fork diff
//! reviewable against its upstream.

pub(crate) use tisvlay_core::*;
pub(crate) use tvnext_client::*;
