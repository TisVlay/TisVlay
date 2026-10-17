//! Pairing-wizard state and async flow drivers.
//!
//! Holds the discovery sidecar (Rust-side source-of-truth + the Slint
//! model derived from it), the schema-prefetch + autofill pipeline,
//! and the pair-flow drivers that route progress updates and typed
//! per-field errors back into the wizard's UI surface.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gettextrs::gettext;
use parking_lot::Mutex;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tisvlay::{
    DaemonClient, LinkMacOutcome, LinkMacReport, PairStarted, PairTerminal, PairingProgress,
};
use tisvlay::{FailureCode, PairingPhase};
use tracing::warn;

use crate::edit_pane::extract_invalid_vendor_config;
use crate::schema_form::{AutofilledRow, autofill_schema, compute_schema_valid};
use crate::{AppWindow, PairingFieldRow, PairingWizard, WizardDiscoveredTv, error_text, on_ui};

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
        // Brand for the vendor tile: the backend's resolved device vendor
        // (`vendor_data["brand"]`, e.g. CEC's `lg`) when known, else the
        // backend vendor id. Mirrors `TvInfo::brand` on the TVs pane.
        let brand = self
            .vendor_data
            .get("brand")
            .filter(|b| !b.is_empty())
            .unwrap_or(&self.vendor);
        // The daemon tags a node it could enumerate but not open (e.g. a
        // `/dev/cecN` its user lacks access to) via `vendor_data["access"]`,
        // so the row can warn the user before they try to pair it.
        let access_warning =
            self.vendor_data.get("access").map(String::as_str) == Some("permission_denied");
        // Discovery-source chip label (`SSDP` / `CEC`) from the daemon's
        // `vendor_data["source"]`, upper-cased for display. No vendor-id
        // fallback: a row with no discovery source (e.g. a manually-entered
        // TV) shows no source chip rather than mislabeling the make as a
        // source. `source` is ASCII, so `to_ascii_uppercase` suffices.
        let source_label = self
            .vendor_data
            .get("source")
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_uppercase())
            .unwrap_or_default();
        WizardDiscoveredTv {
            id: SharedString::from(self.id.as_str()),
            name: SharedString::from(self.name.as_str()),
            vendor: SharedString::from(self.vendor.as_str()),
            brand: SharedString::from(brand.as_str()),
            address: SharedString::from(self.address.as_str()),
            hw_model: SharedString::from(self.hw_model.as_str()),
            source_label: SharedString::from(source_label),
            access_warning,
        }
    }
}

/// Shared sidecar holding the wizard's discovery results with full
/// `vendor_data`. Cloned into every closure that mutates or reads
/// the discovery list. Reset to empty whenever the wizard reopens.
pub(crate) type WizardEntries = Arc<Mutex<Vec<WizardEntry>>>;

/// Generation counter for in-flight pair / re-pair tasks.
///
/// Bumped by [`reset_wizard`] (which fires on Cancel and on every
/// fresh wizard open). Each spawned pair-flow task captures the
/// value at start; subsequent `on_ui` callbacks and the final
/// `publish_pair_outcome` no-op if the live counter no longer
/// matches the captured one. This stops a late-arriving
/// `PairingProgress` event (or terminal outcome) from bleeding into
/// a freshly-opened wizard session — the classic "Cancel → reopen
/// quickly" race.
pub(crate) type WizardGen = Arc<AtomicU64>;

/// Cancel handle for the wizard's streaming discovery task.
///
/// The spawn site stores a fresh `oneshot::Sender` here right before
/// `tokio::spawn(run_wizard_discovery(…))` — overwriting any prior
/// sender, which drops it and cancels the prior task. The task
/// `select!`s on the matching `Receiver`, so either an explicit
/// `send(())` or a `drop(Sender)` wakes it. Cancel handlers (and
/// `reset_wizard`) call [`cancel_wizard_discovery`] to take + send
/// the sender explicitly.
pub(crate) type WizardDiscoveryCancel = Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>;

/// Cancel handle for the wizard's manual-entry MAC enrichment
/// (`enrich` rpc) task. Same shape as
/// [`WizardDiscoveryCancel`] but a distinct type so callers can't
/// accidentally swap them — the streaming discovery loop and the
/// one-shot manual-entry probe have different lifecycles.
pub(crate) type WizardManualLookupCancel = Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>;

/// Bundles the wizard callback family's shared handles so each
/// callback clones once instead of cloning each handle individually.
#[derive(Clone)]
pub(crate) struct WizardCtx {
    pub daemon: Arc<DaemonClient>,
    pub entries: WizardEntries,
    pub generation: WizardGen,
    pub discovery_cancel: WizardDiscoveryCancel,
    pub manual_cancel: WizardManualLookupCancel,
}

/// Cancel any in-flight wizard discovery session.
///
/// Takes the cancel sender out of the sidecar (so subsequent calls
/// are no-ops) and `send(())`s it. Send failure (receiver already
/// dropped) is normal — the task may have exited via stream-end or
/// gen-mismatch in the same tick.
pub(crate) fn cancel_wizard_discovery(cancel: &WizardDiscoveryCancel) {
    if let Some(tx) = cancel.lock().take() {
        let _ = tx.send(());
    }
}

/// Cancel any in-flight wizard manual-entry MAC enrichment.
/// Symmetric with [`cancel_wizard_discovery`]; called from
/// `reset_wizard` so closing the wizard mid-lookup signals the
/// spawned task to drop its work.
pub(crate) fn cancel_wizard_manual_lookup(cancel: &WizardManualLookupCancel) {
    if let Some(tx) = cancel.lock().take() {
        let _ = tx.send(());
    }
}

/// The name a discovery pick should leave in the wizard's name field:
/// `default` (the picked entry's name) when the field is empty or still
/// holds what the previous pick auto-filled, `None` when the user typed
/// it — so Back + a different pick doesn't keep the earlier TV's name,
/// and a hand-typed name is never overwritten.
pub(crate) fn picked_tv_name(
    current: &str,
    last_autofilled: &str,
    default: &str,
) -> Option<String> {
    (current.is_empty() || current == last_autofilled).then(|| default.to_string())
}

/// Fill the wizard's name field from a pick per [`picked_tv_name`],
/// remembering the fill so the next pick can replace it.
pub(crate) fn fill_picked_tv_name(wizard: &PairingWizard<'_>, default: &str) {
    if let Some(name) = picked_tv_name(
        &wizard.get_tv_name(),
        &wizard.get_tv_name_autofilled(),
        default,
    ) {
        wizard.set_tv_name(SharedString::from(name.as_str()));
        wizard.set_tv_name_autofilled(SharedString::from(name));
    }
}

/// Push `entries` into the wizard's `discovered` Slint model.
/// Caller has already locked + mutated `entries`; this just rebuilds
/// the visible view from the updated source-of-truth.
pub(crate) fn publish_wizard_entries(wizard: &PairingWizard<'_>, entries: &[WizardEntry]) {
    let rows: Vec<WizardDiscoveredTv> = entries.iter().map(WizardEntry::to_slint).collect();
    wizard.set_discovered(ModelRc::new(VecModel::from(rows)));
}

/// Clear all wizard state and close the wizard. Also bumps the
/// generation counter so any in-flight pair / re-pair task's
/// callbacks become no-ops for this session, and cancels any
/// in-flight streaming discovery and manual-entry MAC enrichment
/// tasks so they stop work immediately.
pub(crate) fn reset_wizard(
    app: &AppWindow,
    wizard_gen: &WizardGen,
    wizard_cancel: &WizardDiscoveryCancel,
    manual_lookup_cancel: &WizardManualLookupCancel,
) {
    // Bump first so any callback that fires *during* the reset
    // sequence below also sees the drift.
    wizard_gen.fetch_add(1, Ordering::SeqCst);
    cancel_wizard_discovery(wizard_cancel);
    cancel_wizard_manual_lookup(manual_lookup_cancel);
    let wizard = app.global::<PairingWizard>();
    wizard.set_active(false);
    wizard.set_step(1);
    wizard.set_selected_index(-1);
    wizard.set_tv_name(SharedString::default());
    wizard.set_tv_name_autofilled(SharedString::default());
    wizard.set_mac_notice(SharedString::default());
    wizard.set_mac_reported(SharedString::default());
    wizard.set_mac_configured(SharedString::default());
    wizard.set_mac_error(SharedString::default());
    wizard.set_system_profile(SharedString::default());
    wizard.set_managed(true);
    // Reset to the alpha-first vendor (index 0) — `supported-vendors`
    // is sorted by display name in `state_watch::initial_fetch_and_watch`.
    // The vendor ThemedSelect derives its shown value from this index and
    // writes picks back through its `selected-index` callback.
    wizard.set_manual_vendor_index(0);
    wizard.set_manual_ip(SharedString::default());
    wizard.set_selected_vendor(SharedString::default());
    wizard.set_selected_vendor_name(SharedString::default());
    wizard.set_selected_address(SharedString::default());
    wizard.set_selected_hw_model(SharedString::default());
    wizard.set_discovery_streaming(false);
    wizard.set_discovery_error(SharedString::default());
    wizard.set_manual_lookup_streaming(false);
    wizard.set_pair_phase(SharedString::default());
    wizard.set_pair_message(SharedString::default());
    wizard.set_pair_tv_id(SharedString::default());
    wizard.set_discovered(ModelRc::new(VecModel::<WizardDiscoveredTv>::default()));
    wizard.set_schema_fields(ModelRc::new(VecModel::<PairingFieldRow>::default()));
    wizard.set_schema_valid(false);
}

/// Run a streaming discovery session against the daemon.
///
/// Subscribes to the `discovery/*/found/*` keys first, then calls
/// the `discover-start` rpc so the daemon loops M-SEARCH bursts
/// at its configured `poll_interval` (default 8 s) until we stop it.
/// Each newly-seen TV is appended to `wizard_entries` and the Slint
/// model is refreshed.
///
/// Exits on any of:
/// - `cancel_rx` resolving (explicit cancel via
///   [`cancel_wizard_discovery`] or sidecar replacement on a new
///   session)
/// - the `wizard_gen` counter drifting from the value captured at
///   start (defense in depth against late callbacks bleeding into a
///   reopened wizard)
/// - the findings stream ending (daemon-side hard cap or daemon
///   shutdown)
///
/// Always calls the `discover-stop` rpc on exit, even if cancel
/// happened mid-iteration, so the daemon promptly stops scanning.
pub(crate) async fn run_wizard_discovery(
    daemon: Arc<DaemonClient>,
    wizard_entries: WizardEntries,
    wizard_gen: WizardGen,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
    app_weak: slint::Weak<AppWindow>,
) {
    use futures_util::StreamExt;

    let gen_at_start = wizard_gen.load(Ordering::SeqCst);

    // Subscribe BEFORE start_discovery to avoid losing the first burst.
    let signal_stream = match daemon.discovery_found_stream().await {
        Ok(s) => s,
        Err(e) => {
            publish_discovery_error(&app_weak, &e);
            return;
        }
    };

    // The session runs until stopped or the daemon's hard cap, its
    // safety net for a crashed GUI.
    let session_id = match daemon.start_discovery().await {
        Ok(sid) => sid,
        Err(e) => {
            publish_discovery_error(&app_weak, &e);
            return;
        }
    };

    tokio::pin!(signal_stream);

    loop {
        tokio::select! {
            biased;
            _ = &mut cancel_rx => break,
            maybe_item = signal_stream.next() => {
                let Some(found) = maybe_item else {
                    // Stream ended — daemon shutdown or subscriber dropped.
                    break;
                };
                if wizard_gen.load(Ordering::SeqCst) != gen_at_start {
                    break;
                }
                let entry = WizardEntry {
                    id: found.id,
                    name: found.name,
                    vendor: found.vendor,
                    address: found.address,
                    hw_model: found
                        .hardware_id
                        .and_then(|hardware| hardware.model)
                        .unwrap_or_default(),
                    vendor_data: found.vendor_data,
                };
                // Daemon dedups within a single session; across
                // overlapping sessions (rare — wizard typically only
                // has one open) the same TV could arrive again, so
                // dedup on (vendor, id) here too.
                let mut entries = wizard_entries.lock();
                if entries
                    .iter()
                    .any(|e| e.vendor == entry.vendor && e.id == entry.id)
                {
                    continue;
                }
                entries.push(entry);
                let snapshot = entries.clone();
                drop(entries);
                on_ui(&app_weak, move |app| {
                    let wizard = app.global::<PairingWizard>();
                    publish_wizard_entries(&wizard, &snapshot);
                });
            }
        }
    }

    // Tell the daemon to stop scanning. Best-effort — log on error.
    if let Err(e) = daemon.stop_discovery(session_id).await {
        warn!(session_id = %session_id, error = %e, "stop_discovery failed");
    }

    // Turn off the spinner unless the wizard has already been reset
    // (the new session owns the spinner state now).
    on_ui(&app_weak, move |app| {
        if wizard_gen.load(Ordering::SeqCst) != gen_at_start {
            return;
        }
        let wizard = app.global::<PairingWizard>();
        wizard.set_discovery_streaming(false);
    });
}

/// Surface a discovery setup error in the wizard's error banner.
fn publish_discovery_error(app_weak: &slint::Weak<AppWindow>, e: &tisvlay::Error) {
    let msg = error_text(e);
    on_ui(app_weak, move |app| {
        let wizard = app.global::<PairingWizard>();
        wizard.set_discovery_streaming(false);
        wizard.set_discovery_error(SharedString::from(msg));
    });
}

/// Fetch the vendor's pairing-form schema and publish autofilled
/// rows into the wizard's `schema-fields` model. Failure leaves
/// the model empty and surfaces the error in `discovery-error`
/// (the closest existing inline-message slot — a dedicated
/// `Errors.pairing` channel is on the roadmap).
///
/// `manual_entry: true` tells the helper-override logic that this
/// schema fetch was triggered by the wizard's manual-add path
/// (rather than picking from the SSDP-discovered list). When set,
/// fields whose autofill missed get a context-specific helper —
/// today, the `mac` field's helper is prefixed with "we tried,
/// didn't find it", so the user understands what happened rather
/// than seeing a silent empty field.
pub(crate) async fn fetch_and_publish_schema(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    vendor: String,
    entry: WizardEntry,
    manual_entry: bool,
) {
    match daemon.get_pairing_schema(&vendor).await {
        Ok(schema) => {
            let mut raw_rows = autofill_schema(&schema, &entry.vendor_data, &entry.address);
            if manual_entry {
                apply_manual_entry_helper_overrides(&mut raw_rows);
            }
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
            let msg = error_text(&e);
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_discovery_error(SharedString::from(msg));
            });
        }
    }
}

/// Prefix the schema's field-level helper with context for the
/// manual-entry path when the MAC enrichment didn't resolve a value.
/// The prefix composes with the vendor's own helper rather than
/// replacing it: the wizard knows *which UX path* the user is on, but
/// only the vendor knows what it can do about a blank MAC (LG reads it
/// from the TV after pairing; a vendor that can't says so itself).
fn apply_manual_entry_helper_overrides(rows: &mut [AutofilledRow]) {
    for row in rows.iter_mut() {
        if row.name == tisvlay_core::MAC_CONFIG_KEY && row.value.is_empty() {
            row.helper = gettext("MAC not detected. {0}").replace("{0}", &row.helper);
        }
    }
}

/// Run a `pair` rpc flow against the daemon's async-spawn
/// contract:
///
/// - `pair_tv` returns the reserved tv_id immediately (sync
///   validation only). Errors here route via the typed-error
///   field-mapper (`extract_invalid_vendor_config`) so per-field
///   issues bounce back to step 2.
/// - The handshake outcome surfaces on the `pairing/<uuid>/progress`
///   keys. We drain the stream filtered on our tv_id to a terminal
///   `complete`/`failed` via the shared
///   `DaemonClient::drain_pair_to_terminal` helper.
/// - Every `on_ui` callback (intermediate phase updates and the
///   final outcome publish) checks the captured generation against
///   the live `wizard_gen`. A user who clicks Cancel + reopens the
///   wizard before this task finishes won't get stale phase
///   messages bleeding into the new session.
pub(crate) async fn run_wizard_pair(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    wizard_gen: WizardGen,
    name: String,
    vendor: String,
    config: serde_json::Map<String, serde_json::Value>,
    managed: bool,
) {
    // Capture the generation at task start. Every callback below
    // re-reads the live gen and skips if it has drifted.
    let captured_gen = wizard_gen.load(Ordering::SeqCst);

    // Subscribe BEFORE the pair rpc so we don't miss any phase
    // updates (the daemon may publish `awaiting_tv` within
    // microseconds of `pair_tv` returning).
    let stream = match daemon.pairing_progress_stream().await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "PairingProgress subscribe failed; falling back to phase-less pair");
            run_pair_no_progress(
                daemon,
                app_weak,
                wizard_gen,
                captured_gen,
                name,
                vendor,
                config,
                managed,
            )
            .await;
            return;
        }
    };

    // `pair_tv` is sync-validation-only — it returns the reserved
    // tv_id immediately. Errors here are unknown-vendor /
    // ConfigInvalidVendorConfig / etc. — route via the field-mapper
    // so per-field problems bounce back to step 2.
    let PairStarted { id: tv_id, attempt } =
        match daemon.pair_tv(&name, &vendor, config, managed).await {
            Ok(started) => started,
            Err(e) => {
                publish_pair_outcome(
                    app_weak,
                    &wizard_gen,
                    captured_gen,
                    Err(PairError::Rpc(e)),
                    true,
                );
                return;
            }
        };

    // Set tv_id on the wizard so the Cancel button can route the
    // `cancel-pair` rpc. The same pair_tv_id is consumed
    // by the cancel handler in `lib.rs::on_cancel`.
    {
        let tv_id_for_ui = tv_id.to_string();
        let app_weak = app_weak.clone();
        let wizard_gen = wizard_gen.clone();
        on_ui(&app_weak, move |app| {
            if wizard_gen.load(Ordering::SeqCst) != captured_gen {
                return;
            }
            app.global::<PairingWizard>()
                .set_pair_tv_id(SharedString::from(tv_id_for_ui.as_str()));
        });
    }

    // Drain to terminal, dispatching non-terminal phases into the
    // wizard's phase/message fields with the gen-guarded callback.
    let on_progress = make_progress_callback(app_weak.clone(), wizard_gen.clone(), captured_gen);
    let outcome = DaemonClient::drain_pair_to_terminal(stream, tv_id, attempt, on_progress).await;

    let result = terminal_to_result(outcome);
    publish_pair_outcome(app_weak, &wizard_gen, captured_gen, result, true);
}

/// Fallback pair flow when the PairingProgress subscription fails
/// to set up. The `pair_tv` RPC is async-spawn — its `Ok(tv_id)`
/// return only means "the daemon spawned the handshake", not
/// "the TV is paired." Without a progress stream we have no
/// reliable signal to determine the actual outcome, so we
/// convert RPC-success into an explicit error rather than risk
/// showing a `complete` phase that lets the user click Done over
/// a pair that may still be in flight or fail. Sync-validation
/// errors (unknown vendor, ConfigInvalidVendorConfig) still
/// route through their normal field-mapping path.
#[expect(
    clippy::too_many_arguments,
    reason = "a spawned task: the client handles plus the pair request's fields, each moved in once from the caller"
)]
async fn run_pair_no_progress(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    wizard_gen: WizardGen,
    captured_gen: u64,
    name: String,
    vendor: String,
    config: serde_json::Map<String, serde_json::Value>,
    managed: bool,
) {
    let result = match daemon.pair_tv(&name, &vendor, config, managed).await {
        Err(e) => Err(PairError::Rpc(e)),
        Ok(_) => {
            let msg = gettext(
                "Pairing started but progress signal is unavailable. Check the TV list and retry the wizard to confirm.",
            );
            Err(PairError::Local(msg))
        }
    };
    publish_pair_outcome(app_weak, &wizard_gen, captured_gen, result, false);
}

/// Run a `re-pair` rpc call with live progress updates streamed
/// into the wizard's `pair_phase` / `pair_message` fields. Parallel
/// to [`run_wizard_pair`] but for the re-pair flow:
///
/// - No name / vendor / config arguments — the daemon refreshes
///   the existing TV's credentials in place. The wizard's
///   StepPair UI was pre-filled with display-only labels by the
///   `on_request_re_pair` handler.
/// - tv_id is the caller-provided argument (we already know it), and
///   the re-pair rpc returns the session's attempt — the drain filters
///   on both, so the previous session's replayed `complete` can't end
///   it.
/// - Errors route through the same `publish_pair_outcome` path,
///   so re-pair surfaces failures with the same UI as the pair flow.
pub(crate) async fn run_wizard_re_pair(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    wizard_gen: WizardGen,
    tv_id: uuid::Uuid,
) {
    let captured_gen = wizard_gen.load(Ordering::SeqCst);

    let stream = match daemon.pairing_progress_stream().await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "PairingProgress subscribe failed; falling back to phase-less re-pair");
            // Same reasoning as `run_pair_no_progress`: re_pair's
            // `Ok(())` only means "spawn succeeded" under the
            // async-spawn contract. Without a progress stream we
            // can't know the actual outcome, so we surface an
            // explicit failure instead of an optimistic `complete`.
            let result = match daemon.re_pair(tv_id).await {
                Err(e) => Err(PairError::Rpc(e)),
                Ok(_) => {
                    let msg = gettext(
                        "Pairing started but progress signal is unavailable. Check the TV list and retry the wizard to confirm.",
                    );
                    Err(PairError::Local(msg))
                }
            };
            publish_pair_outcome(app_weak, &wizard_gen, captured_gen, result, false);
            return;
        }
    };

    // Re-pair's sync RPC errors are TvNotFound / AlreadyPairing —
    // route them through the same outcome publisher.
    let attempt = match daemon.re_pair(tv_id).await {
        Ok(attempt) => attempt,
        Err(e) => {
            publish_pair_outcome(
                app_weak,
                &wizard_gen,
                captured_gen,
                Err(PairError::Rpc(e)),
                true,
            );
            return;
        }
    };

    let on_progress = make_progress_callback(app_weak.clone(), wizard_gen.clone(), captured_gen);
    let outcome = DaemonClient::drain_pair_to_terminal(stream, tv_id, attempt, on_progress).await;

    let result = terminal_to_result(outcome);
    publish_pair_outcome(app_weak, &wizard_gen, captured_gen, result, true);
}

/// Build the `on_progress` callback shared by the pair and re-pair
/// drain loops. Each non-terminal phase update routes into the
/// wizard's phase/message Slint properties, gated by the generation
/// counter so late updates from a cancelled-and-reopened wizard
/// session are dropped.
pub(crate) fn make_progress_callback(
    app_weak: slint::Weak<AppWindow>,
    wizard_gen: WizardGen,
    captured_gen: u64,
) -> impl FnMut(PairingPhase, &str) + Send + 'static {
    move |phase: PairingPhase, message: &str| {
        if wizard_gen.load(Ordering::SeqCst) != captured_gen {
            return;
        }
        let phase = phase.as_str();
        let message = message.to_string();
        let app_weak = app_weak.clone();
        let wizard_gen = wizard_gen.clone();
        on_ui(&app_weak, move |app| {
            // Re-check inside the closure too: the outer check
            // happens *before* `on_ui` schedules onto the Slint
            // event loop; if the user clicks Cancel (bumping
            // `wizard_gen`) between scheduling and the loop
            // running this closure, we'd otherwise stomp the new
            // session with a stale phase/message. Matches the
            // double-guard pattern elsewhere in this file.
            if wizard_gen.load(Ordering::SeqCst) != captured_gen {
                return;
            }
            let wizard = app.global::<PairingWizard>();
            wizard.set_pair_phase(SharedString::from(phase));
            wizard.set_pair_message(SharedString::from(message));
        });
    }
}

/// Typed pair/re-pair outcome for [`publish_pair_outcome`]: the
/// handshake failure's detail and the SDK error both stay typed, so an
/// `invalid-field` refusal routes to its schema row.
pub(crate) enum PairError {
    /// Terminal `failed` phase from the daemon — localize by its `code`.
    Handshake(Box<PairingProgress>),
    /// A sync-rpc or stream error (unknown vendor, `invalid-field`, the
    /// progress stream ending early) — field-routed, else shown with its
    /// cause chain.
    Rpc(tisvlay::Error),
    /// A GUI-side failure, already localized (the no-progress fallback).
    Local(String),
}

/// Localize a pairing-failure reason for display. Branches on the
/// daemon's stable, machine-readable `code`; an unknown or absent code
/// falls back to the daemon's English `message`. The daemon stays
/// locale-agnostic — the translated copy lives here in the GUI.
fn localize_pair_failure(detail: &PairingProgress) -> String {
    if detail.code == Some(FailureCode::DevicePermissionDenied) {
        // Name the actual node from the payload when present.
        let node = detail.path.as_deref().unwrap_or("/dev/cec*");
        return gettext(
            "The TisVlay daemon doesn't have permission to use {0}. Add its user to the group \
             that owns the device (on NixOS, enable services.tisvlay.cec), or install the udev \
             rule granting access.",
        )
        .replace("{0}", node);
    }
    detail.message.clone()
}

/// Translate `drain_pair_to_terminal`'s `Result<PairTerminal>` into the
/// shape [`publish_pair_outcome`] expects: a `Complete` keeps its link-MAC
/// report for the done step, a terminal `Failed` keeps its typed detail,
/// and a stream error stays the SDK's.
fn terminal_to_result(
    outcome: tisvlay::Result<PairTerminal>,
) -> Result<Option<LinkMacReport>, PairError> {
    match outcome {
        Ok(PairTerminal::Complete { link_mac, .. }) => Ok(link_mac),
        Ok(PairTerminal::Failed { progress, .. }) => Err(PairError::Handshake(Box::new(progress))),
        Err(e) => Err(PairError::Rpc(e)),
    }
}

/// The done step's notice for the MAC the TV reported at pairing, as
/// (`mac-notice`, reported, configured): "filled" when the daemon stored
/// it, "differs" when an entered MAC was kept, and no notice for a match
/// or no report.
fn mac_notice(report: Option<&LinkMacReport>) -> (&'static str, String, String) {
    match report {
        Some(LinkMacReport {
            reported,
            outcome: LinkMacOutcome::Filled,
        }) => ("filled", reported.clone(), String::new()),
        Some(LinkMacReport {
            reported,
            outcome: LinkMacOutcome::Differs { configured },
        }) => ("differs", reported.clone(), configured.clone()),
        Some(LinkMacReport {
            outcome: LinkMacOutcome::Matches,
            ..
        })
        | None => ("", String::new(), String::new()),
    }
}

/// Push the pair-flow outcome into the wizard. Routes the daemon's
/// `invalid-field` refusals back to the originating schema row so the user sees the inline
/// red-text error directly under the offending field, then bounces
/// back to step 2 for correction. Anything else surfaces as a
/// generic step-3 failure.
///
/// Gated by the generation counter: if the wizard has been reset
/// (and `wizard_gen` bumped) since the captured value, the
/// callback no-ops so a cancelled-and-reopened wizard isn't
/// stomped by a late terminal outcome from the prior session.
pub(crate) fn publish_pair_outcome(
    app_weak: slint::Weak<AppWindow>,
    wizard_gen: &WizardGen,
    captured_gen: u64,
    result: Result<Option<LinkMacReport>, PairError>,
    had_progress_stream: bool,
) {
    let wizard_gen = wizard_gen.clone();
    on_ui(&app_weak, move |app| {
        if wizard_gen.load(Ordering::SeqCst) != captured_gen {
            return;
        }
        let wizard = app.global::<PairingWizard>();
        match result {
            Ok(report) => {
                let (notice, reported, configured) = mac_notice(report.as_ref());
                wizard.set_mac_notice(notice.into());
                wizard.set_mac_reported(SharedString::from(reported));
                wizard.set_mac_configured(SharedString::from(configured));
                wizard.set_mac_error(SharedString::default());
                wizard.set_pair_phase(PairingPhase::Complete.as_str().into());
                if wizard.get_pair_message().is_empty() {
                    let msg = if had_progress_stream {
                        gettext("Client key stored.")
                    } else {
                        gettext("Paired successfully.")
                    };
                    wizard.set_pair_message(SharedString::from(msg));
                }
            }
            // Typed handshake failure (carries a reason code) — localize
            // by the code. A handshake failure is never an `invalid-field`
            // refusal, so no field routing.
            Err(PairError::Handshake(detail)) => {
                wizard.set_pair_phase(PairingPhase::Failed.as_str().into());
                wizard.set_pair_message(SharedString::from(localize_pair_failure(&detail)));
            }
            Err(PairError::Rpc(e)) => {
                if let Some((field, reason)) = extract_invalid_vendor_config(&e)
                    && mark_field_invalid(&wizard, &field, &reason)
                {
                    // Field-level routing succeeded — bounce
                    // back to step 2 so the inline error sits
                    // under the offending widget.
                    wizard.set_step(2);
                    wizard.set_pair_phase(SharedString::default());
                    wizard.set_pair_message(SharedString::default());
                    return;
                }
                wizard.set_pair_phase(PairingPhase::Failed.as_str().into());
                wizard.set_pair_message(SharedString::from(error_text(&e)));
            }
            Err(PairError::Local(message)) => {
                wizard.set_pair_phase(PairingPhase::Failed.as_str().into());
                wizard.set_pair_message(SharedString::from(message));
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use super::{
        AutofilledRow, WizardEntry, apply_manual_entry_helper_overrides, fill_picked_tv_name,
        localize_pair_failure, mark_field_invalid, picked_tv_name, publish_wizard_entries,
        reset_wizard,
    };
    use crate::{AppWindow, PairingFieldRow, PairingWizard};
    use parking_lot::Mutex;
    use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
    use tisvlay::{
        FailureCode, LinkMacOutcome, LinkMacReport, PairTerminal, PairingPhase, PairingProgress,
    };
    use tokio::sync::oneshot;

    fn field_row(name: &str) -> PairingFieldRow {
        PairingFieldRow {
            name: SharedString::from(name),
            label: SharedString::default(),
            helper: SharedString::default(),
            field_type: SharedString::from("string"),
            required: true,
            placeholder: SharedString::default(),
            options: ModelRc::new(VecModel::<SharedString>::default()),
            value: SharedString::from("value"),
            state: SharedString::from("valid"),
            error: SharedString::default(),
            autofilled: false,
        }
    }

    #[test]
    fn mark_field_invalid_marks_matching_row_and_invalidates_schema() {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");
        let wizard = app.global::<PairingWizard>();
        let model = ModelRc::new(VecModel::from(vec![field_row("ip")]));
        wizard.set_schema_fields(model.clone());
        wizard.set_schema_valid(true);

        assert!(mark_field_invalid(&wizard, "ip", "not a valid address"));

        let row = model.row_data(0).expect("the schema row remains present");
        assert_eq!(row.state.as_str(), "invalid");
        assert_eq!(row.error.as_str(), "not a valid address");
        assert!(!wizard.get_schema_valid());
    }

    #[test]
    fn mark_field_invalid_returns_false_for_unknown_field() {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");
        let wizard = app.global::<PairingWizard>();
        let model = ModelRc::new(VecModel::from(vec![field_row("ip")]));
        wizard.set_schema_fields(model.clone());
        wizard.set_schema_valid(true);

        assert!(!mark_field_invalid(
            &wizard,
            "unknown",
            "not a valid address"
        ));

        let row = model.row_data(0).expect("the schema row remains present");
        assert_eq!(row.state.as_str(), "valid");
        assert!(row.error.is_empty());
        assert!(wizard.get_schema_valid());
    }

    #[test]
    fn publish_wizard_entries_projects_brand_source_and_access_warning() {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");
        let wizard = app.global::<PairingWizard>();
        let entries = vec![
            WizardEntry {
                id: "cec-1".into(),
                name: "Office TV".into(),
                vendor: "cec".into(),
                address: "/dev/cec0".into(),
                hw_model: "Mock CEC".into(),
                vendor_data: HashMap::from([
                    ("brand".into(), "lg".into()),
                    ("source".into(), "cec".into()),
                    ("access".into(), "permission_denied".into()),
                ]),
            },
            WizardEntry {
                id: "manual-1".into(),
                name: "Manual TV".into(),
                vendor: "mock".into(),
                ..Default::default()
            },
        ];

        publish_wizard_entries(&wizard, &entries);

        let rows = wizard.get_discovered();
        assert_eq!(rows.row_count(), 2);
        let discovered = rows.row_data(0).expect("discovered row is projected");
        assert_eq!(discovered.brand, "lg");
        assert_eq!(discovered.source_label, "CEC");
        assert!(discovered.access_warning);
        let manual = rows.row_data(1).expect("manual row is projected");
        assert_eq!(manual.brand, "mock");
        assert!(manual.source_label.is_empty());
        assert!(!manual.access_warning);
    }

    #[test]
    fn reset_wizard_bumps_generation_cancels_tasks_and_clears_state() {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");
        let wizard = app.global::<PairingWizard>();
        wizard.set_active(true);
        wizard.set_step(3);
        wizard.set_selected_index(2);
        wizard.set_tv_name(SharedString::from("Living Room"));
        wizard.set_tv_name_autofilled(SharedString::from("Old TV"));
        wizard.set_mac_notice(SharedString::from("differs"));
        wizard.set_mac_reported(SharedString::from("84:90:0A:63:A9:94"));
        wizard.set_mac_configured(SharedString::from("64:E4:A5:6C:AB:B4"));
        wizard.set_mac_error(SharedString::from("MAC lookup failed"));
        wizard.set_system_profile(SharedString::from("profile-1"));
        wizard.set_managed(false);
        wizard.set_manual_vendor_index(3);
        wizard.set_manual_ip(SharedString::from("192.0.2.10"));
        wizard.set_selected_vendor(SharedString::from("lg"));
        wizard.set_selected_vendor_name(SharedString::from("LG WebOS"));
        wizard.set_selected_address(SharedString::from("192.0.2.20"));
        wizard.set_selected_hw_model(SharedString::from("OLED42C3"));
        wizard.set_discovery_streaming(true);
        wizard.set_discovery_error(SharedString::from("discovery failed"));
        wizard.set_manual_lookup_streaming(true);
        wizard.set_pair_phase(SharedString::from("awaiting_tv"));
        wizard.set_pair_message(SharedString::from("waiting for TV"));
        wizard.set_pair_tv_id(SharedString::from("tv-1"));
        wizard.set_discovered(ModelRc::new(VecModel::from(vec![
            crate::WizardDiscoveredTv {
                name: SharedString::from("Living Room"),
                ..Default::default()
            },
        ])));
        wizard.set_schema_fields(ModelRc::new(VecModel::from(vec![field_row("ip")])));
        wizard.set_schema_valid(true);

        let generation = Arc::new(AtomicU64::new(7));
        let (discovery_tx, mut discovery_rx) = oneshot::channel();
        let (lookup_tx, mut lookup_rx) = oneshot::channel();
        let discovery_cancel = Arc::new(Mutex::new(Some(discovery_tx)));
        let lookup_cancel = Arc::new(Mutex::new(Some(lookup_tx)));

        reset_wizard(&app, &generation, &discovery_cancel, &lookup_cancel);

        assert_eq!(generation.load(Ordering::SeqCst), 8);
        assert!(discovery_rx.try_recv().is_ok());
        assert!(lookup_rx.try_recv().is_ok());
        assert!(!wizard.get_active());
        assert_eq!(wizard.get_step(), 1);
        assert_eq!(wizard.get_selected_index(), -1);
        assert!(wizard.get_tv_name().is_empty());
        assert!(wizard.get_tv_name_autofilled().is_empty());
        assert!(wizard.get_mac_notice().is_empty());
        assert!(wizard.get_mac_reported().is_empty());
        assert!(wizard.get_mac_configured().is_empty());
        assert!(wizard.get_mac_error().is_empty());
        assert!(wizard.get_system_profile().is_empty());
        assert!(wizard.get_managed());
        assert_eq!(wizard.get_manual_vendor_index(), 0);
        assert!(wizard.get_manual_ip().is_empty());
        assert!(wizard.get_selected_vendor().is_empty());
        assert!(wizard.get_selected_vendor_name().is_empty());
        assert!(wizard.get_selected_address().is_empty());
        assert!(wizard.get_selected_hw_model().is_empty());
        assert!(!wizard.get_discovery_streaming());
        assert!(wizard.get_discovery_error().is_empty());
        assert!(!wizard.get_manual_lookup_streaming());
        assert!(wizard.get_pair_phase().is_empty());
        assert!(wizard.get_pair_message().is_empty());
        assert!(wizard.get_pair_tv_id().is_empty());
        assert_eq!(wizard.get_discovered().row_count(), 0);
        assert_eq!(wizard.get_schema_fields().row_count(), 0);
        assert!(!wizard.get_schema_valid());
    }

    /// The done step speaks only when there's something to act on or know:
    /// a filled MAC, or an entered one that differs from the TV's.
    #[test]
    fn mac_notice_per_outcome() {
        use super::{LinkMacOutcome, LinkMacReport, mac_notice};
        let report = |outcome| LinkMacReport {
            reported: "84:90:0A:63:A9:94".into(),
            outcome,
        };
        assert_eq!(
            mac_notice(Some(&report(LinkMacOutcome::Filled))),
            ("filled", "84:90:0A:63:A9:94".into(), String::new())
        );
        assert_eq!(
            mac_notice(Some(&report(LinkMacOutcome::Differs {
                configured: "64:E4:A5:6C:AB:B4".into()
            }))),
            (
                "differs",
                "84:90:0A:63:A9:94".into(),
                "64:E4:A5:6C:AB:B4".into()
            )
        );
        assert_eq!(
            mac_notice(Some(&report(LinkMacOutcome::Matches))),
            ("", String::new(), String::new())
        );
        assert_eq!(mac_notice(None), ("", String::new(), String::new()));
    }

    #[test]
    fn pair_terminal_results_preserve_complete_and_failure_details() {
        use super::{PairError, terminal_to_result};

        let report = LinkMacReport {
            reported: "84:90:0A:63:A9:94".into(),
            outcome: LinkMacOutcome::Filled,
        };
        let Ok(complete) = terminal_to_result(Ok(PairTerminal::Complete {
            tv: uuid::Uuid::nil(),
            message: "paired".into(),
            link_mac: Some(report.clone()),
        })) else {
            panic!("complete terminal succeeds")
        };
        assert_eq!(complete, Some(report));

        let progress = PairingProgress {
            attempt: uuid::Uuid::nil(),
            phase: PairingPhase::Failed,
            message: "permission denied".into(),
            code: Some(FailureCode::DevicePermissionDenied),
            path: Some("/dev/cec0".into()),
            link_mac: None,
        };
        let failed = terminal_to_result(Ok(PairTerminal::Failed {
            tv: uuid::Uuid::nil(),
            progress: progress.clone(),
        }))
        .expect_err("failed terminal becomes a handshake error");
        assert!(matches!(failed, PairError::Handshake(detail) if *detail == progress));

        let rpc_error = tisvlay::Error::Timeout {
            service: tisvlay::Service::Daemon,
        };
        assert!(matches!(
            terminal_to_result(Err(rpc_error)),
            Err(PairError::Rpc(tisvlay::Error::Timeout {
                service: tisvlay::Service::Daemon
            }))
        ));
    }

    #[test]
    fn device_permission_failure_names_the_node_or_uses_the_fallback() {
        let detail = |path| PairingProgress {
            attempt: uuid::Uuid::nil(),
            phase: PairingPhase::Failed,
            message: "permission denied".into(),
            code: Some(FailureCode::DevicePermissionDenied),
            path,
            link_mac: None,
        };
        assert!(localize_pair_failure(&detail(Some("/dev/cec0".into()))).contains("/dev/cec0"));
        assert!(localize_pair_failure(&detail(None)).contains("/dev/cec*"));

        let unknown = PairingProgress {
            code: Some(FailureCode::PairingRejected),
            ..detail(None)
        };
        assert_eq!(localize_pair_failure(&unknown), "permission denied");
    }

    /// A missed MAC lookup prefixes the vendor's own helper rather than
    /// replacing it, so the vendor's note about a blank MAC survives.
    #[test]
    fn missed_mac_prefixes_the_vendor_helper() {
        let vendor_helper =
            "Required for Wake-on-LAN. Leave blank to read it from the TV after pairing.";
        let mut rows = vec![
            AutofilledRow {
                name: "mac".into(),
                helper: vendor_helper.into(),
                ..Default::default()
            },
            AutofilledRow {
                name: "ip".into(),
                helper: "TV address.".into(),
                value: String::new(),
                ..Default::default()
            },
        ];
        apply_manual_entry_helper_overrides(&mut rows);
        assert_eq!(rows[0].helper, format!("MAC not detected. {vendor_helper}"));
        assert_eq!(rows[1].helper, "TV address.");

        // A MAC that did resolve keeps the plain vendor helper.
        let mut found = vec![AutofilledRow {
            name: "mac".into(),
            helper: vendor_helper.into(),
            value: "84:90:0A:63:A9:94".into(),
            ..Default::default()
        }];
        apply_manual_entry_helper_overrides(&mut found);
        assert_eq!(found[0].helper, vendor_helper);
    }

    #[test]
    fn pick_fills_an_empty_name() {
        assert_eq!(picked_tv_name("", "", "Den TV"), Some("Den TV".into()));
    }

    #[test]
    fn repick_replaces_the_previous_autofill() {
        // Picked the 55", went Back, picked the 42": the 55"'s name goes.
        assert_eq!(
            picked_tv_name("OLED55G3PUA", "OLED55G3PUA", "OLED42C3PUA"),
            Some("OLED42C3PUA".into())
        );
    }

    #[test]
    fn repick_keeps_a_hand_typed_name() {
        assert_eq!(picked_tv_name("Office", "OLED55G3PUA", "OLED42C3PUA"), None);
    }

    #[test]
    fn fill_picked_name_tracks_autofill_without_overwriting_manual_text() {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");
        let wizard = app.global::<PairingWizard>();

        fill_picked_tv_name(&wizard, "Living Room");
        assert_eq!(wizard.get_tv_name(), "Living Room");
        assert_eq!(wizard.get_tv_name_autofilled(), "Living Room");

        fill_picked_tv_name(&wizard, "Bedroom");
        assert_eq!(wizard.get_tv_name(), "Bedroom");
        assert_eq!(wizard.get_tv_name_autofilled(), "Bedroom");

        wizard.set_tv_name(SharedString::from("Office"));
        fill_picked_tv_name(&wizard, "Kitchen");
        assert_eq!(wizard.get_tv_name(), "Office");
        assert_eq!(wizard.get_tv_name_autofilled(), "Bedroom");
    }
}
