//! The profile editor's cards render every row kind Rust sends them.

use i_slint_backend_testing::ElementQuery;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use tisvlay_gui::{AppWindow, Pane, ProfileFieldEntry, ProfileKind, ProfileRow, Profiles};

fn count_labeled(app: &AppWindow, label: &str) -> usize {
    let label = label.to_string();
    ElementQuery::from_root(app)
        .match_descendants()
        .match_predicate(move |e| e.accessible_label().as_deref() == Some(label.as_str()))
        .find_all()
        .len()
}

/// The FROM THE TV card shows a switch row (`remote_keys`), not only its
/// dropdown rows.
#[test]
fn the_from_the_tv_card_shows_its_switch_row() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().expect("AppWindow::new");
    app.set_active_pane(Pane::Profiles);
    let profiles = app.global::<Profiles>();
    profiles.set_selected_kind(ProfileKind::System);
    profiles.set_selected_row(ProfileRow {
        uuid: SharedString::from("00000000-0000-0000-0000-000000005159"),
        ..ProfileRow::default()
    });
    let label = "Remote keys reach the desktop";
    profiles.set_editor_from_tv_fields(ModelRc::new(VecModel::from(vec![ProfileFieldEntry {
        field_name: SharedString::from("remote_keys"),
        label: SharedString::from(label),
        kind: SharedString::from("bool"),
        ..ProfileFieldEntry::default()
    }])));
    assert!(count_labeled(&app, label) > 0, "the switch row is missing");
}
