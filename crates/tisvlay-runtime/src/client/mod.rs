//! Client-side helpers the GUI and tray share: dispatch coalescing,
//! optimistic-state reconciliation, and the profile-reference
//! projection.

mod coalescer;
mod profile_refs;
mod reconciler;

pub use coalescer::Coalescer;
pub use profile_refs::{TvAssignment, references_of, user_references_of};
pub use reconciler::{Reconciler, ReconcilerMap};
