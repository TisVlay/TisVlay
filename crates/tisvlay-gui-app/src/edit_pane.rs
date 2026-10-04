//! TVs-pane edit-mode helpers: edit-form validity and server-side
//! per-field error routing over the wire's typed rpc errors.
//!
//! `extract_invalid_vendor_config` is shared with the pairing
//! wizard's `mark_field_invalid` flow — both surfaces consume the
//! same daemon-side validation error so per-field reasons round-trip
//! back to the originating widget.

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::schema_form::compute_schema_valid;
use crate::{AppWindow, EditPane, PairingFieldRow, on_ui};
use tisvlay::ErrorCode;

/// The `(field, reason)` of the daemon's `invalid-field` refusal, so the
/// form can mark the offending row. `None` for any other error (network,
/// pairing rejected, …): callers surface those in their generic slot.
pub(crate) fn extract_invalid_vendor_config(err: &tisvlay::Error) -> Option<(String, String)> {
    err.invalid_field()
        .map(|(field, reason)| (field.to_string(), reason.to_string()))
}

/// True when the daemon refused with `pairing-not-found`: a cancel-pair
/// call raced normal completion. Callers downgrade the log level instead
/// of treating it as a real failure.
pub(crate) fn is_not_pairing(err: &tisvlay::Error) -> bool {
    err.rpc_code() == Some(ErrorCode::PairingNotFound)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc_err(code: ErrorCode, message: &str) -> tisvlay::Error {
        tisvlay::Error::Rpc {
            code,
            message: message.into(),
        }
    }

    #[test]
    fn extract_invalid_vendor_config_routes_field_and_reason() {
        let err = rpc_err(ErrorCode::InvalidField, "ip: required");
        assert_eq!(
            extract_invalid_vendor_config(&err),
            Some(("ip".into(), "required".into()))
        );
    }

    #[test]
    fn extract_invalid_vendor_config_ignores_other_codes() {
        let err = rpc_err(ErrorCode::BadRequest, "vendor config: expected a table");
        assert_eq!(extract_invalid_vendor_config(&err), None);
        let timeout = tisvlay::Error::Timeout {
            service: tisvlay::Service::Daemon,
        };
        assert_eq!(extract_invalid_vendor_config(&timeout), None);
    }

    #[test]
    fn is_not_pairing_matches_the_cancel_race_code() {
        assert!(is_not_pairing(&rpc_err(
            ErrorCode::PairingNotFound,
            "no session"
        )));
        assert!(!is_not_pairing(&rpc_err(ErrorCode::BadRequest, "nope")));
    }

    #[test]
    fn reason_halves_keep_their_colons() {
        let err = rpc_err(ErrorCode::InvalidField, "ip: not an IP address: ::1::bad");
        assert_eq!(
            extract_invalid_vendor_config(&err),
            Some(("ip".into(), "not an IP address: ::1::bad".into()))
        );
    }
}
