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

    #[test]
    fn mark_edit_field_invalid_marks_matching_row_and_invalidates_form() {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");
        let edit_pane = app.global::<EditPane>();
        let model = ModelRc::new(VecModel::from(vec![field_row("ip")]));
        edit_pane.set_name(SharedString::from("Living Room"));
        edit_pane.set_fields(model.clone());
        edit_pane.set_valid(true);

        assert!(mark_edit_field_invalid(
            &edit_pane,
            &model,
            "ip",
            "not a valid address"
        ));

        let row = model.row_data(0).expect("the schema row remains present");
        assert_eq!(row.state.as_str(), "invalid");
        assert_eq!(row.error.as_str(), "not a valid address");
        assert!(!edit_pane.get_valid());
    }

    #[test]
    fn mark_edit_field_invalid_returns_false_for_unknown_field() {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");
        let edit_pane = app.global::<EditPane>();
        let model = ModelRc::new(VecModel::from(vec![field_row("ip")]));
        edit_pane.set_name(SharedString::from("Living Room"));
        edit_pane.set_fields(model.clone());
        edit_pane.set_valid(true);

        assert!(!mark_edit_field_invalid(
            &edit_pane,
            &model,
            "unknown",
            "not a valid address"
        ));

        let row = model.row_data(0).expect("the schema row remains present");
        assert_eq!(row.state.as_str(), "valid");
        assert!(row.error.is_empty());
        assert!(edit_pane.get_valid());
    }

    #[test]
    fn worker_ui_updates_apply_on_the_event_loop() {
        i_slint_backend_testing::init_integration_test_with_system_time();
        let app = AppWindow::new().expect("AppWindow::new");
        let edit_pane = app.global::<EditPane>();
        edit_pane.set_saving(true);
        let profiles = app.global::<crate::Profiles>();
        profiles.set_selected_kind(crate::ProfileKind::System);
        profiles.set_selected_uuid(SharedString::from("profile-1"));
        profiles.set_editor_fields(ModelRc::new(VecModel::from(vec![
            crate::ProfileFieldEntry {
                field_name: SharedString::from("on_startup"),
                ..Default::default()
            },
        ])));
        profiles.set_editor_can_reset_all(true);
        let tv_id = SharedString::from("tv-1");
        app.set_tvs(ModelRc::new(VecModel::from(vec![crate::TvViewModel {
            uuid: tv_id.clone(),
            ..Default::default()
        }])));

        publish_edit_error(&app.as_weak(), "schema unavailable".into());
        crate::push_tvs_error(
            &app.as_weak(),
            "Power on failed".into(),
            "TV is unavailable".into(),
        );
        crate::power_cycle::update_power_cycle_phase(
            &app.as_weak(),
            tv_id.as_str(),
            Some(tisvlay::PowerPhase::Connecting),
        );
        slint::quit_event_loop().expect("quit the test event loop");
        slint::run_event_loop().expect("drain the test event loop");

        assert_eq!(edit_pane.get_error().as_str(), "schema unavailable");
        assert!(!edit_pane.get_saving());
        let tv_error = app.global::<crate::Errors>().get_tvs();
        assert_eq!(tv_error.title, "Power on failed");
        assert_eq!(tv_error.message, "TV is unavailable");
        assert_eq!(tv_error.tone, "error");
        assert_eq!(
            app.get_tvs().row_data(0).unwrap().power_cycle_phase,
            "connecting"
        );

        crate::power_cycle::update_power_cycle_phase(&app.as_weak(), tv_id.as_str(), None);
        crate::profiles::clear_selection_in_slint(app.as_weak());
        slint::quit_event_loop().expect("quit after clearing the power phase");
        slint::run_event_loop().expect("drain the phase-clear event");
        assert!(
            app.get_tvs()
                .row_data(0)
                .unwrap()
                .power_cycle_phase
                .is_empty()
        );
        assert_eq!(profiles.get_selected_kind(), crate::ProfileKind::None);
        assert!(profiles.get_selected_uuid().is_empty());
        assert!(profiles.get_selected_row().uuid.is_empty());
        assert_eq!(profiles.get_editor_fields().row_count(), 0);
        assert!(!profiles.get_editor_can_reset_all());
    }
}
