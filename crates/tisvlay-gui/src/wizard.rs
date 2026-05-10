//! Pairing-wizard state and async flow drivers.
//!
//! Holds the discovery sidecar (Rust-side source-of-truth + the Slint
//! model derived from it), the schema-prefetch + autofill pipeline,
//! and the pair-flow drivers that route progress updates and typed
//! per-field errors back into the wizard's UI surface.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gettextrs::gettext;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tisvlay_core::DaemonClient;
use tracing::warn;

use crate::edit_pane::extract_invalid_vendor_config;
use crate::schema_form::{AutofilledRow, autofill_schema, compute_schema_valid};
use crate::{AppWindow, PairingFieldRow, PairingWizard, WizardDiscoveredTv, on_ui};

/// One row of the pairing-wizard's discovery list, owned Rust-side.
///
/// The wizard's Slint model (`[WizardDiscoveredTv]`) is a derived
/// view over a `Vec<WizardEntry>`. Slint structs can't carry a map,
/// so `vendor_data` lives here and the Slint side renders only the
/// scalar columns (id / name / vendor / address / hw_model). When
/// the user advances to step 2 the Rust side reads `vendor_data`
/// out of the selected entry and seeds the schema-driven form's
/// autofill values.
#[derive(Clone, Debug, Default)]
pub(crate) struct WizardEntry {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub address: String,
    pub hw_model: String,
    pub vendor_data: HashMap<String, String>,
}

impl WizardEntry {
    fn to_slint(&self) -> WizardDiscoveredTv {
        WizardDiscoveredTv {
            id: SharedString::from(self.id.as_str()),
            name: SharedString::from(self.name.as_str()),
            vendor: SharedString::from(self.vendor.as_str()),
            address: SharedString::from(self.address.as_str()),
            hw_model: SharedString::from(self.hw_model.as_str()),
        }
    }
}

/// Shared sidecar holding the wizard's discovery results with full
/// `vendor_data`. Cloned into every closure that mutates or reads
/// the discovery list. Reset to empty whenever the wizard reopens.
pub(crate) type WizardEntries = Arc<Mutex<Vec<WizardEntry>>>;

/// Push `entries` into the wizard's `discovered` Slint model.
/// Caller has already locked + mutated `entries`; this just rebuilds
/// the visible view from the updated source-of-truth.
pub(crate) fn publish_wizard_entries(wizard: &PairingWizard<'_>, entries: &[WizardEntry]) {
    let rows: Vec<WizardDiscoveredTv> = entries.iter().map(WizardEntry::to_slint).collect();
    wizard.set_discovered(ModelRc::new(VecModel::from(rows)));
}

/// Clear all wizard state and close the wizard.
pub(crate) fn reset_wizard(app: &AppWindow) {
    let wizard = app.global::<PairingWizard>();
    wizard.set_active(false);
    wizard.set_step(1);
    wizard.set_selected_index(-1);
    wizard.set_tv_name(SharedString::default());
    wizard.set_system_profile(SharedString::default());
    wizard.set_managed(true);
    // Reset to the alpha-first vendor (index 0) — `supported-vendors`
    // is sorted by display name in `state_watch::initial_fetch_and_watch`.
    // The ComboBox's `current-index <=> manual-vendor-index` binding
    // updates the visible selection in lockstep. See
    // `docs/plans/vendor-state-single-property.md`.
    wizard.set_manual_vendor_index(0);
    wizard.set_manual_ip(SharedString::default());
    wizard.set_selected_vendor(SharedString::default());
    wizard.set_selected_vendor_name(SharedString::default());
    wizard.set_selected_address(SharedString::default());
    wizard.set_selected_hw_model(SharedString::default());
    wizard.set_discovery_streaming(false);
    wizard.set_discovery_error(SharedString::default());
    wizard.set_pair_phase(SharedString::default());
    wizard.set_pair_message(SharedString::default());
    wizard.set_discovered(ModelRc::new(VecModel::<WizardDiscoveredTv>::default()));
    wizard.set_schema_fields(ModelRc::new(VecModel::<PairingFieldRow>::default()));
    wizard.set_schema_valid(false);
}

/// Drive a single discovery scan and stream results into the wizard's
/// `wizard_discovered` model. Subscribes to `Manager1.DiscoveryFound`
/// before invoking `discover_tvs` so signals emitted during the call
/// are captured. v1: the daemon emits in a burst right before the
/// RPC returns, but the subscription pattern is forward-compatible.
pub(crate) async fn run_wizard_discovery(
    daemon: Arc<DaemonClient>,
    wizard_entries: WizardEntries,
    app_weak: slint::Weak<AppWindow>,
) {
    // v1: the daemon currently emits DiscoveryFound signals in a burst
    // right before discover_tvs returns, so subscribing here adds no
    // streaming benefit yet — we just take the synchronous RPC return.
    // When the daemon refactors to per-backend streaming, swap this
    // for a discovery_found_stream() drainer (the helper exists on
    // DaemonClient; the wire shape is forward-compatible).
    match daemon.discover_tvs(5000).await {
        Ok(tvs) => {
            let entries: Vec<WizardEntry> = tvs
                .into_iter()
                .map(
                    |(id, name, address, hw_id, vendor, vendor_data)| WizardEntry {
                        id,
                        name,
                        vendor,
                        address,
                        hw_model: hw_id.model.unwrap_or_default(),
                        vendor_data,
                    },
                )
                .collect();
            // Replace the Rust-owned source-of-truth before Slint sees
            // anything — the Slint view is derived from it.
            if let Ok(mut e) = wizard_entries.lock() {
                *e = entries.clone();
            }
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                publish_wizard_entries(&wizard, &entries);
                wizard.set_discovery_streaming(false);
            });
        }
        Err(e) => {
            let msg = format!("{e}");
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_discovery_streaming(false);
                wizard.set_discovery_error(SharedString::from(msg));
            });
        }
    }
}

/// Fetch the vendor's pairing-form schema and publish autofilled
/// rows into the wizard's `schema-fields` model. Failure leaves
/// the model empty and surfaces the error in `discovery-error`
/// (the closest existing inline-message slot — a dedicated
/// `Errors.pairing` channel is on the roadmap).
pub(crate) async fn fetch_and_publish_schema(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    vendor: String,
    entry: WizardEntry,
) {
    match daemon.get_pairing_schema(&vendor).await {
        Ok(schema) => {
            let raw_rows = autofill_schema(&schema, &entry.vendor_data, &entry.address);
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                let rows: Vec<PairingFieldRow> = raw_rows
                    .into_iter()
                    .map(AutofilledRow::into_slint)
                    .collect();
                let model: ModelRc<PairingFieldRow> = ModelRc::new(VecModel::from(rows));
                let valid = compute_schema_valid(&model);
                wizard.set_schema_fields(model);
                wizard.set_schema_valid(valid);
            });
        }
        Err(e) => {
            warn!(vendor = %vendor, error = %e, "GetPairingSchema failed");
            let msg = format!("{e}");
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_discovery_error(SharedString::from(msg));
            });
        }
    }
}

/// Run a `pair_tv` call with live progress updates streamed into the
/// wizard's `wizard_pair_phase` / `wizard_pair_message` fields.
/// Mirrors `run_power_on_with_progress` but for the pairing flow.
pub(crate) async fn run_wizard_pair(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    name: String,
    vendor: String,
    config_json: String,
    managed: bool,
) {
    use futures_util::StreamExt;

    // Subscribe before issuing the call so we don't miss the first
    // phase (typically `awaiting_tv` on LG).
    let stream = match daemon.pairing_progress_stream().await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "PairingProgress subscribe failed; falling back to phase-less pair");
            // Fall through and just call pair_tv without progress updates.
            run_pair_no_progress(daemon, app_weak, name, vendor, config_json, managed).await;
            return;
        }
    };

    let drainer_weak = app_weak.clone();
    let drainer = tokio::spawn(async move {
        let mut s = std::pin::pin!(stream);
        while let Some((_tv_id, phase, message)) = s.next().await {
            on_ui(&drainer_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_pair_phase(SharedString::from(phase));
                wizard.set_pair_message(SharedString::from(message));
            });
        }
    });

    let result = daemon.pair_tv(&name, &vendor, &config_json, managed).await;
    drainer.abort();

    publish_pair_outcome(app_weak, result, true);
}

/// Fallback pair flow when the PairingProgress subscription fails to
/// set up. Just calls pair_tv synchronously and reflects the final
/// success/failure outcome in the wizard.
async fn run_pair_no_progress(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    name: String,
    vendor: String,
    config_json: String,
    managed: bool,
) {
    let result = daemon.pair_tv(&name, &vendor, &config_json, managed).await;
    publish_pair_outcome(app_weak, result, false);
}

/// Push the pair-flow outcome into the wizard. Routes typed
/// per-field errors (`org.tisvlay.Error.ConfigInvalidVendorConfig`)
/// back to the originating schema row so the user sees the inline
/// red-text error directly under the offending field, then bounces
/// back to step 2 for correction. Anything else surfaces as a
/// generic step-3 failure.
fn publish_pair_outcome(
    app_weak: slint::Weak<AppWindow>,
    result: anyhow::Result<String>,
    had_progress_stream: bool,
) {
    on_ui(&app_weak, move |app| {
        let wizard = app.global::<PairingWizard>();
        match result {
            Ok(_) => {
                wizard.set_pair_phase("complete".into());
                if wizard.get_pair_message().is_empty() {
                    let msg = if had_progress_stream {
                        gettext("Client key stored.")
                    } else {
                        gettext("Paired successfully.")
                    };
                    wizard.set_pair_message(SharedString::from(msg));
                }
            }
            Err(e) => {
                if let Some((field, reason)) = extract_invalid_vendor_config(&e) {
                    if mark_field_invalid(&wizard, &field, &reason) {
                        // Field-level routing succeeded — bounce
                        // back to step 2 so the inline error sits
                        // under the offending widget.
                        wizard.set_step(2);
                        wizard.set_pair_phase(SharedString::default());
                        wizard.set_pair_message(SharedString::default());
                        return;
                    }
                }
                wizard.set_pair_phase("failed".into());
                wizard.set_pair_message(SharedString::from(format!("{e}")));
            }
        }
    });
}

/// Find the schema row whose `name` matches and mark it invalid
/// with the given reason. Returns true if a matching row was
/// found (so the caller can route back to step 2); false means
/// the daemon flagged a field name we don't have, and the caller
/// should fall through to a generic pair-failed surface.
fn mark_field_invalid(wizard: &PairingWizard<'_>, field: &str, reason: &str) -> bool {
    let model = wizard.get_schema_fields();
    let Some(vec_model) = model.as_any().downcast_ref::<VecModel<PairingFieldRow>>() else {
        return false;
    };
    for i in 0..model.row_count() {
        let Some(mut row) = model.row_data(i) else {
            continue;
        };
        if row.name == field {
            row.state = SharedString::from("invalid");
            row.error = SharedString::from(reason);
            vec_model.set_row_data(i, row);
            wizard.set_schema_valid(compute_schema_valid(&model));
            return true;
        }
    }
    false
}
