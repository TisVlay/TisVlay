//! TVs-pane edit-mode helpers: edit-form validity and server-side
//! per-field error routing over the wire's typed rpc errors.
//!
//! `extract_invalid_vendor_config` is shared with the pairing
//! wizard's `mark_field_invalid` flow — both surfaces consume the
//! same daemon-side validation error so per-field reasons round-trip
//! back to the originating widget.

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::compat::RpcError;
use crate::schema_form::compute_schema_valid;
use crate::{AppWindow, EditPane, PairingFieldRow, on_ui};
use tisvlay_wire::error::ErrorCode;

/// Parse an `invalid-field` error message into `(field_name,
/// reason)`. The daemon emits the message as `"<field>: <reason>"`;
/// split on the first `": "` so reasons carrying further colons
/// (e.g. IPv6 addresses) stay intact in the reason half.
fn parse_invalid_vendor_config(msg: &str) -> Option<(String, String)> {
    let (field, reason) = msg.split_once(": ")?;
    if field.is_empty() {
        return None;
    }
    Some((field.to_string(), reason.to_string()))
}

/// Walk an `anyhow::Error` chain looking for a typed [`RpcError`]
/// with the given wire code. Returns its message on match. Underpins
/// the per-error helpers below — clients route on the stable code
/// rather than the display string.
fn find_rpc_error(err: &anyhow::Error, code: ErrorCode) -> Option<String> {
    err.chain()
        .filter_map(|cause| match cause.downcast_ref::<RpcError>() {
            Some(RpcError::Service(error)) => Some(error),
            _ => None,
        })
        .find(|error| error.code == code)
        .map(|error| error.message.clone())
}

/// Inspect an `anyhow::Error` chain for the daemon's `invalid-field`
/// rpc error and, if found, extract the `(field, reason)` pair.
/// Returns `None` for any other error (network, pairing-rejected,
/// etc.) so callers can fall back to surface the message in the
/// wizard's generic pair-failed slot.
pub(crate) fn extract_invalid_vendor_config(err: &anyhow::Error) -> Option<(String, String)> {
    let detail = find_rpc_error(err, ErrorCode::InvalidField)?;
    parse_invalid_vendor_config(&detail)
}

/// True when the error chain carries the daemon's `pairing-not-found`
/// rpc error — i.e., a cancel-pair call raced normal completion.
/// Callers downgrade the log level instead of treating it as a real
/// failure.
pub(crate) fn is_not_pairing(err: &anyhow::Error) -> bool {
    find_rpc_error(err, ErrorCode::PairingNotFound).is_some()
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

    fn rpc_err(code: ErrorCode, message: &str) -> anyhow::Error {
        anyhow::Error::new(RpcError::Service(tisvlay_wire::error::WireError {
            code,
            message: message.into(),
        }))
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
    fn extract_invalid_vendor_config_survives_context_wrapping() {
        // Callers add context above the SDK error; the chain walk must
        // still find the typed cause underneath.
        let err =
            rpc_err(ErrorCode::InvalidField, "mac: not a MAC address").context("saving TV config");
        assert_eq!(
            extract_invalid_vendor_config(&err),
            Some(("mac".into(), "not a MAC address".into()))
        );
    }

    #[test]
    fn extract_invalid_vendor_config_ignores_other_codes() {
        let err = rpc_err(ErrorCode::BadRequest, "vendor config: expected a table");
        assert_eq!(extract_invalid_vendor_config(&err), None);
        assert_eq!(
            extract_invalid_vendor_config(&anyhow::anyhow!("plain")),
            None
        );
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
