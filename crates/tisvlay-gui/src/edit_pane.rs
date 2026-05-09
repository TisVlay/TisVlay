//! TVs-pane edit-mode helpers: edit-form validity, server-side
//! per-field error routing, and the `org.tisvlay.Error.ConfigInvalid
//! VendorConfig` parser.
//!
//! `parse_invalid_vendor_config` + `extract_invalid_vendor_config`
//! are shared with the pairing wizard's `mark_field_invalid` flow —
//! both surfaces consume the same daemon-side validation error so
//! per-field reasons round-trip back to the originating widget.

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::schema_form::compute_schema_valid;
use crate::{AppWindow, EditPane, PairingFieldRow, on_ui};

/// Parse a `ConfigInvalidVendorConfig` D-Bus error message into
/// `(field_name, reason)`. The daemon emits the message as
/// `"<field>: <reason>"` (per `DaemonError::ConfigInvalidVendorConfig`
/// in `tisvlayd::dbus::mod`); split on the first `": "` so reasons
/// carrying further colons (e.g. IPv6 addresses) stay intact in the
/// reason half.
fn parse_invalid_vendor_config(msg: &str) -> Option<(String, String)> {
    let (field, reason) = msg.split_once(": ")?;
    if field.is_empty() {
        return None;
    }
    Some((field.to_string(), reason.to_string()))
}

/// Inspect an `anyhow::Error` chain for the daemon's typed
/// `org.tisvlay.Error.ConfigInvalidVendorConfig` D-Bus error and,
/// if found, extract the `(field, reason)` pair. Returns `None`
/// for any other error (network, pairing-rejected, etc.) so callers
/// can fall back to surface the message in the wizard's generic
/// pair-failed slot.
pub(crate) fn extract_invalid_vendor_config(err: &anyhow::Error) -> Option<(String, String)> {
    let mut cur: &dyn std::error::Error = err.as_ref();
    loop {
        if let Some(zbus::Error::MethodError(name, detail, _)) = cur.downcast_ref::<zbus::Error>()
            && name.as_str() == "org.tisvlay.Error.ConfigInvalidVendorConfig"
        {
            return parse_invalid_vendor_config(detail.as_deref().unwrap_or(""));
        }
        match cur.source() {
            Some(s) => cur = s,
            None => return None,
        }
    }
}

/// Edit-form gate. Mirrors the wizard's [`compute_schema_valid`]
/// guard but adds the friendly-name leg.
///
/// Both legs matter: an empty `name` is rejected even though there's
/// no schema row for it (TVs always have a friendly name), and any
/// invalid / required-but-empty schema row fails the same way it
/// does in the wizard.
pub(crate) fn compute_edit_valid(edit_pane: &EditPane<'_>) -> bool {
    !edit_pane.get_name().is_empty() && compute_schema_valid(&edit_pane.get_fields())
}

/// Route a server-side per-field validation error back to the
/// originating row in the edit pane's schema-fields model.
///
/// Mirrors `mark_field_invalid` for the wizard side. Returns true
/// if the field name matched a row (so the caller can suppress the
/// generic error banner). On a name-mismatch returns false and the
/// caller falls through to the generic surface.
pub(crate) fn mark_edit_field_invalid(
    edit_pane: &EditPane<'_>,
    model: &ModelRc<PairingFieldRow>,
    field: &str,
    reason: &str,
) -> bool {
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
            edit_pane.set_valid(compute_edit_valid(edit_pane));
            return true;
        }
    }
    false
}

/// Push a generic edit-mode error message onto the EditPane banner
/// from a worker thread. Called when the prefetch flow (GetPairingSchema
/// / GetConfig) fails before the form is even populated.
pub(crate) fn publish_edit_error(app_weak: &slint::Weak<AppWindow>, message: String) {
    on_ui(app_weak, move |app| {
        let edit_pane = app.global::<EditPane>();
        edit_pane.set_error(SharedString::from(message));
        edit_pane.set_saving(false);
    });
}
