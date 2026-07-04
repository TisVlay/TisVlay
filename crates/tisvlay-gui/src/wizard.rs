//! Pairing-wizard state and async flow drivers.
//!
//! Holds the discovery sidecar (Rust-side source-of-truth + the Slint
//! model derived from it), the schema-prefetch + autofill pipeline,
//! and the pair-flow drivers that route progress updates and typed
//! per-field errors back into the wizard's UI surface.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::compat::{DaemonClient, PairTerminal, PairingProgressDetail};
use gettextrs::gettext;
use parking_lot::Mutex;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tisvlay_core::errors::DeviceErrorCode;
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
            publish_discovery_error(&app_weak, e);
            return;
        }
    };

    // `timeout_ms = 0` ⇒ until-stop-or-hard-cap semantics. The hard
    // cap is the daemon's safety net for a crashed GUI.
    let session_id = match daemon.start_discovery(0).await {
        Ok(sid) => sid,
        Err(e) => {
            publish_discovery_error(&app_weak, e);
            return;
        }
    };

    tokio::pin!(signal_stream);

    loop {
        tokio::select! {
            biased;
            _ = &mut cancel_rx => break,
            maybe_item = signal_stream.next() => {
                let Some((id, name, address, hw_id, vendor, vendor_data)) = maybe_item else {
                    // Stream ended — daemon shutdown or subscriber dropped.
                    break;
                };
                if wizard_gen.load(Ordering::SeqCst) != gen_at_start {
                    break;
                }
                let entry = WizardEntry {
                    id,
                    name,
                    vendor,
                    address,
                    hw_model: hw_id.model.unwrap_or_default(),
                    vendor_data,
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
    if let Err(e) = daemon.stop_discovery(&session_id).await {
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
fn publish_discovery_error(app_weak: &slint::Weak<AppWindow>, e: impl std::fmt::Display) {
    let msg = format!("{e}");
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
/// today, the `mac` field's "Required for Wake-on-LAN" line is
/// replaced with a clearer "we tried, didn't find it" message so
/// the user understands what happened rather than seeing a silent
/// empty field.
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
            let msg = format!("{e}");
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_discovery_error(SharedString::from(msg));
            });
        }
    }
}

/// Swap the schema's generic field-level helper text for context-
/// specific guidance when the manual-entry MAC enrichment didn't
/// resolve a value. Lives wizard-side rather than as a schema
/// extension because the override is purely about *which UX path*
/// the user is on — the schema itself stays vendor-owned. Today
/// only the `mac` field gets an override; structure makes it easy
/// to add others as the manual-entry story grows.
fn apply_manual_entry_helper_overrides(rows: &mut [AutofilledRow]) {
    for row in rows.iter_mut() {
        if row.name == "mac" && row.value.is_empty() {
            row.helper = gettext(
                "MAC not detected. Enter it manually to enable Wake-on-LAN \
                 (otherwise the TV won't power on from full-off).",
            );
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
    config_json: String,
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
                config_json,
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
    let tv_id = match daemon.pair_tv(&name, &vendor, &config_json, managed).await {
        Ok(id) => id,
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
        let tv_id_for_ui = tv_id.clone();
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
    let outcome = DaemonClient::drain_pair_to_terminal(stream, &tv_id, on_progress).await;

    let result = terminal_to_result(outcome, tv_id);
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
#[allow(clippy::too_many_arguments)]
async fn run_pair_no_progress(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    wizard_gen: WizardGen,
    captured_gen: u64,
    name: String,
    vendor: String,
    config_json: String,
    managed: bool,
) {
    let result = match daemon.pair_tv(&name, &vendor, &config_json, managed).await {
        Err(e) => Err(PairError::Rpc(e)),
        Ok(_) => {
            let msg = gettext(
                "Pairing started but progress signal is unavailable. Check the TV list and retry the wizard to confirm.",
            );
            Err(PairError::Rpc(anyhow::anyhow!("{msg}")))
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
/// - tv_id is the caller-provided argument (we already know it),
///   so the drain filter is straightforward — no "first signal
///   wins" heuristic needed.
/// - Errors route through the same `publish_pair_outcome` path,
///   so re-pair surfaces failures with the same UI as the pair flow.
pub(crate) async fn run_wizard_re_pair(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    wizard_gen: WizardGen,
    tv_id: String,
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
            let result = match daemon.re_pair(&tv_id).await {
                Err(e) => Err(PairError::Rpc(e)),
                Ok(()) => {
                    let msg = gettext(
                        "Pairing started but progress signal is unavailable. Check the TV list and retry the wizard to confirm.",
                    );
                    Err(PairError::Rpc(anyhow::anyhow!("{msg}")))
                }
            };
            publish_pair_outcome(app_weak, &wizard_gen, captured_gen, result, false);
            return;
        }
    };

    // Re-pair's sync RPC errors are TvNotFound / AlreadyPairing —
    // route them through the same outcome publisher.
    if let Err(e) = daemon.re_pair(&tv_id).await {
        publish_pair_outcome(
            app_weak,
            &wizard_gen,
            captured_gen,
            Err(PairError::Rpc(e)),
            true,
        );
        return;
    }

    let on_progress = make_progress_callback(app_weak.clone(), wizard_gen.clone(), captured_gen);
    let outcome = DaemonClient::drain_pair_to_terminal(stream, &tv_id, on_progress).await;

    let result = terminal_to_result(outcome, tv_id);
    publish_pair_outcome(app_weak, &wizard_gen, captured_gen, result, true);
}

/// Build the `on_progress` callback shared by the pair and re-pair
/// drain loops. Each non-terminal phase update routes into the
/// wizard's phase/message Slint properties, gated by the generation
/// counter so late updates from a cancelled-and-reopened wizard
/// session are dropped.
fn make_progress_callback(
    app_weak: slint::Weak<AppWindow>,
    wizard_gen: WizardGen,
    captured_gen: u64,
) -> impl FnMut(&str, &str) + Send + 'static {
    move |phase: &str, message: &str| {
        if wizard_gen.load(Ordering::SeqCst) != captured_gen {
            return;
        }
        let phase = phase.to_string();
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

/// Typed pair/re-pair outcome for [`publish_pair_outcome`]. Keeps the
/// handshake failure's typed detail compiler-visible (no anyhow
/// erase-then-downcast) while still carrying sync-RPC errors as `anyhow`
/// — those need the `zbus::Error::MethodError` chain intact for
/// `extract_invalid_vendor_config`'s per-field routing.
enum PairError {
    /// Terminal `failed` phase from the daemon — localize by its `code`.
    Handshake(PairingProgressDetail),
    /// A sync-RPC / stream error (unknown vendor, ConfigInvalidVendorConfig,
    /// no-progress fallback) — field-routed, else shown verbatim.
    Rpc(anyhow::Error),
}

/// Localize a pairing-failure reason for display. Branches on the
/// daemon's stable, machine-readable `code`; an unknown or absent code
/// falls back to the daemon's English `message`. The daemon stays
/// locale-agnostic — the translated copy lives here in the GUI.
fn localize_pair_failure(detail: &PairingProgressDetail) -> String {
    if detail.code.as_deref() == Some(DeviceErrorCode::PermissionDenied.as_str()) {
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
/// shape [`publish_pair_outcome`] expects: a terminal `Failed` keeps its
/// typed detail; a stream error stays `anyhow`.
fn terminal_to_result(
    outcome: anyhow::Result<PairTerminal>,
    tv_id: String,
) -> Result<String, PairError> {
    match outcome {
        Ok(PairTerminal::Complete { .. }) => Ok(tv_id),
        Ok(PairTerminal::Failed { detail, .. }) => Err(PairError::Handshake(detail)),
        Err(e) => Err(PairError::Rpc(e)),
    }
}

/// Push the pair-flow outcome into the wizard. Routes typed
/// per-field errors (`org.tisvlay.Error.ConfigInvalidVendorConfig`)
/// back to the originating schema row so the user sees the inline
/// red-text error directly under the offending field, then bounces
/// back to step 2 for correction. Anything else surfaces as a
/// generic step-3 failure.
///
/// Gated by the generation counter: if the wizard has been reset
/// (and `wizard_gen` bumped) since the captured value, the
/// callback no-ops so a cancelled-and-reopened wizard isn't
/// stomped by a late terminal outcome from the prior session.
fn publish_pair_outcome(
    app_weak: slint::Weak<AppWindow>,
    wizard_gen: &WizardGen,
    captured_gen: u64,
    result: Result<String, PairError>,
    had_progress_stream: bool,
) {
    let wizard_gen = wizard_gen.clone();
    on_ui(&app_weak, move |app| {
        if wizard_gen.load(Ordering::SeqCst) != captured_gen {
            return;
        }
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
            // Typed handshake failure (carries a reason code) — localize
            // by the code. A handshake failure never carries the
            // ConfigInvalidVendorConfig D-Bus shape, so no field routing.
            Err(PairError::Handshake(detail)) => {
                wizard.set_pair_phase("failed".into());
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
