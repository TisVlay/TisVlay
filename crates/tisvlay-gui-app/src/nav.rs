//! Browser-style back/forward navigation for the sidebar panes.
//!
//! Every pane change funnels through one Rust-owned history stack so
//! Alt+Left/Right (a top-level `FocusScope` in `main.slint`) and the
//! mouse back/forward buttons (a winit event hook in `lib.rs`) can
//! replay it. Each entry remembers the pane *and* the master/detail
//! selection active there, so going back restores the exact TV or
//! profile you were viewing — including after a cross-pane click-through
//! (the Profiles "Used by" chip → a TV; a TV's profile pencil → a
//! profile).
//!
//! [`wire`] returns the [`NavHandles`] the rest of the app needs:
//! `lib.rs`'s two click-throughs record history through `go_*`, and the
//! winit mouse-button hook replays it through `back` / `forward`. The
//! `apply` closure here is the only writer of `AppWindow.active-pane`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use slint::{ComponentHandle, Model, SharedString};
use tisvlay::{DaemonClient, SessionClient};
use tokio::sync::RwLock;
use tracing::warn;

use crate::profiles::{self, EditorKind, ProfileEditor};
use crate::{AppWindow, Pane, ProfileKind, ProfileRow, Profiles};

/// The master/detail selection captured alongside a pane.
#[derive(Clone, PartialEq)]
enum NavSel {
    /// Settings / About — nothing to restore.
    None,
    /// TVs pane — selected TV by uuid (empty = nothing selected).
    Tv(SharedString),
    /// Profiles pane — selected (kind, uuid).
    Profile(ProfileKind, SharedString),
}

struct NavEntry {
    pane: Pane,
    sel: NavSel,
}

struct NavHistory {
    entries: Vec<NavEntry>,
    cursor: usize,
}

impl NavHistory {
    fn navigate(&mut self, app: &AppWindow, pane: Pane, sel: NavSel) -> bool {
        let cur = self.cursor;
        let cur_pane = self.entries[cur].pane;
        self.entries[cur].sel = read_current_sel(app, cur_pane);
        if self.entries[cur].pane == pane && self.entries[cur].sel == sel {
            return false;
        }
        self.entries.truncate(cur + 1);
        self.entries.push(NavEntry { pane, sel });
        self.cursor = self.entries.len() - 1;
        true
    }

    fn step(&mut self, app: &AppWindow, forward: bool) -> Option<(Pane, NavSel)> {
        let cur = self.cursor;
        let next = if forward {
            (cur + 1 < self.entries.len()).then_some(cur + 1)?
        } else {
            cur.checked_sub(1)?
        };
        let cur_pane = self.entries[cur].pane;
        self.entries[cur].sel = read_current_sel(app, cur_pane);
        self.cursor = next;
        Some((self.entries[next].pane, self.entries[next].sel.clone()))
    }
}

// `Rc<dyn Fn>` aliases for the navigation closures — readable call sites
// with clippy's `type_complexity` lint satisfied in one place.
type ApplyFn = Rc<dyn Fn(&AppWindow, Pane, &NavSel)>;
type GoFn = Rc<dyn Fn(&AppWindow, Pane, NavSel)>;
type StepFn = Rc<dyn Fn(&AppWindow, bool)>;
type GoTvFn = Rc<dyn Fn(&AppWindow, SharedString)>;
type GoProfileFn = Rc<dyn Fn(&AppWindow, ProfileKind, SharedString)>;
type NavActionFn = Rc<dyn Fn(&AppWindow)>;

/// Navigation entry points used by `lib.rs`: the cross-pane
/// click-throughs record history through `go_*`, and the winit
/// mouse-button hook replays it through `back` / `forward`.
pub(crate) struct NavHandles {
    pub(crate) go_tv: GoTvFn,
    pub(crate) go_profile: GoProfileFn,
    pub(crate) back: NavActionFn,
    pub(crate) forward: NavActionFn,
}

/// Wire the `navigate` / `nav-back` / `nav-forward` Slint callbacks to a
/// fresh history seeded at the current pane, and hand back the entry
/// points the rest of the app drives navigation through.
pub(crate) fn wire(
    app: &AppWindow,
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    editor: ProfileEditor,
) -> NavHandles {
    let history = Rc::new(RefCell::new(NavHistory {
        entries: vec![NavEntry {
            pane: app.get_active_pane(),
            sel: NavSel::None,
        }],
        cursor: 0,
    }));

    // Set the pane and restore its selection. The sole writer of
    // `active-pane`; never touches history (callers own that).
    let apply: ApplyFn = Rc::new(move |app: &AppWindow, pane: Pane, sel: &NavSel| {
        app.set_active_pane(pane);
        match sel {
            NavSel::Tv(uuid) => app.set_selected_tv_index(resolve_tv_index(app, uuid)),
            NavSel::Profile(kind, uuid) if !uuid.is_empty() => {
                select_profile_in_ui(app, &daemon, &session, &editor, *kind, uuid.clone());
            }
            _ => {}
        }
    });

    // Record a new destination and apply it.
    let go: GoFn = {
        let history = history.clone();
        let apply = apply.clone();
        Rc::new(move |app: &AppWindow, pane: Pane, sel: NavSel| {
            if !history.borrow_mut().navigate(app, pane, sel.clone()) {
                return;
            }
            apply(app, pane, &sel);
        })
    };

    // Move one step back (`forward == false`) or forward through the
    // history, capturing the page being left so its selection is current.
    let step: StepFn = {
        let apply = apply.clone();
        Rc::new(move |app: &AppWindow, forward: bool| {
            if let Some((pane, sel)) = history.borrow_mut().step(app, forward) {
                apply(app, pane, &sel);
            }
        })
    };

    {
        let go = go.clone();
        let weak = app.as_weak();
        app.on_navigate(move |pane| {
            if let Some(app) = weak.upgrade() {
                let sel = read_current_sel(&app, pane);
                go(&app, pane, sel);
            }
        });
    }
    {
        let step = step.clone();
        let weak = app.as_weak();
        app.on_nav_back(move || {
            if let Some(app) = weak.upgrade() {
                step(&app, false);
            }
        });
    }
    {
        let step = step.clone();
        let weak = app.as_weak();
        app.on_nav_forward(move || {
            if let Some(app) = weak.upgrade() {
                step(&app, true);
            }
        });
    }

    let go_tv: GoTvFn = {
        let go = go.clone();
        Rc::new(move |app, uuid| go(app, Pane::Tvs, NavSel::Tv(uuid)))
    };
    let go_profile: GoProfileFn = {
        let go = go.clone();
        Rc::new(move |app, kind, uuid| go(app, Pane::Profiles, NavSel::Profile(kind, uuid)))
    };
    let back: NavActionFn = {
        let step = step.clone();
        Rc::new(move |app| step(app, false))
    };
    let forward: NavActionFn = {
        let step = step.clone();
        Rc::new(move |app| step(app, true))
    };

    NavHandles {
        go_tv,
        go_profile,
        back,
        forward,
    }
}

/// Index of `uuid` in the live TV model, or -1 when empty / not found
/// (e.g. the TV was removed since the entry was recorded).
fn resolve_tv_index(app: &AppWindow, uuid: &SharedString) -> i32 {
    if uuid.is_empty() {
        return -1;
    }
    let tvs = app.get_tvs();
    for i in 0..tvs.row_count() {
        if tvs.row_data(i).is_some_and(|row| row.uuid == *uuid) {
            return i as i32;
        }
    }
    -1
}

/// Read the selection currently live in the UI for `pane`.
fn read_current_sel(app: &AppWindow, pane: Pane) -> NavSel {
    match pane {
        Pane::Tvs => {
            let idx = app.get_selected_tv_index();
            let uuid = if idx >= 0 {
                app.get_tvs()
                    .row_data(idx as usize)
                    .map(|r| r.uuid)
                    .unwrap_or_default()
            } else {
                SharedString::default()
            };
            NavSel::Tv(uuid)
        }
        Pane::Profiles => {
            let p = app.global::<Profiles>();
            NavSel::Profile(p.get_selected_kind(), p.get_selected_uuid())
        }
        _ => NavSel::None,
    }
}

/// Select a profile in the UI: highlight its master-list row + header,
/// then fetch + populate the editor — the same path the master-list
/// click and the TVs-pane profile pencil use. Selection only; `apply`
/// owns the pane flip.
fn select_profile_in_ui(
    app: &AppWindow,
    daemon: &Arc<DaemonClient>,
    session: &Arc<RwLock<Option<SessionClient>>>,
    editor: &ProfileEditor,
    kind: ProfileKind,
    uuid_str: SharedString,
) {
    let Some(editor_kind) = EditorKind::from_slint(kind) else {
        return;
    };
    let Ok(uuid) = uuid::Uuid::parse_str(&uuid_str) else {
        warn!(uuid = %uuid_str, "nav: malformed profile UUID, ignoring");
        return;
    };
    let profiles_g = app.global::<Profiles>();
    let model = match editor_kind {
        EditorKind::System => profiles_g.get_system(),
        EditorKind::User => profiles_g.get_user(),
    };
    let mut matched_row: Option<ProfileRow> = None;
    for i in 0..model.row_count() {
        if let Some(row) = model.row_data(i)
            && row.uuid == uuid_str
        {
            matched_row = Some(row);
            break;
        }
    }
    profiles_g.set_selected_kind(kind);
    profiles_g.set_selected_uuid(uuid_str);
    if let Some(row) = matched_row {
        profiles_g.set_selected_row(row);
    }

    let daemon = daemon.clone();
    let session = session.clone();
    let editor = editor.clone();
    let weak = app.as_weak();
    tokio::spawn(async move {
        profiles::select(editor, daemon, session, weak, editor_kind, uuid).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TvViewModel;
    use slint::{ModelRc, VecModel};

    fn app_with_selected_tv() -> AppWindow {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");
        app.set_tvs(ModelRc::new(VecModel::from(vec![
            TvViewModel {
                uuid: SharedString::from("tv-1"),
                name: SharedString::from("Office"),
                ..Default::default()
            },
            TvViewModel {
                uuid: SharedString::from("tv-2"),
                name: SharedString::from("Bedroom"),
                ..Default::default()
            },
        ])));
        app.set_active_pane(Pane::Tvs);
        app.set_selected_tv_index(1);
        app
    }

    fn history_for(app: &AppWindow) -> NavHistory {
        NavHistory {
            entries: vec![NavEntry {
                pane: app.get_active_pane(),
                sel: NavSel::None,
            }],
            cursor: 0,
        }
    }

    #[test]
    fn resolve_tv_index_handles_found_missing_and_empty_ids() {
        let app = app_with_selected_tv();

        assert_eq!(resolve_tv_index(&app, &SharedString::from("tv-2")), 1);
        assert_eq!(resolve_tv_index(&app, &SharedString::from("missing")), -1);
        assert_eq!(resolve_tv_index(&app, &SharedString::default()), -1);
    }

    #[test]
    fn back_and_forward_restore_the_live_tv_selection() {
        let app = app_with_selected_tv();
        let mut history = history_for(&app);

        assert!(history.navigate(&app, Pane::About, NavSel::None));
        app.set_active_pane(Pane::About);
        assert!(history.step(&app, false).is_some());

        let (pane, selection) = history
            .step(&app, true)
            .unwrap_or_else(|| panic!("forward history should remain after stepping back"));
        assert!(pane == Pane::About);
        assert!(matches!(selection, NavSel::None));

        app.set_active_pane(Pane::Tvs);
        app.set_selected_tv_index(1);
        let (pane, selection) = history
            .step(&app, false)
            .unwrap_or_else(|| panic!("back history should remain after stepping forward"));
        assert!(pane == Pane::Tvs);
        assert!(matches!(selection, NavSel::Tv(uuid) if uuid == "tv-2"));
    }

    #[test]
    fn repeated_destination_is_ignored_and_new_navigation_truncates_forward_history() {
        let app = app_with_selected_tv();
        let mut history = history_for(&app);

        assert!(!history.step(&app, false).is_some());
        assert!(history.navigate(&app, Pane::About, NavSel::None));
        app.set_active_pane(Pane::About);
        assert!(!history.navigate(&app, Pane::About, NavSel::None));
        assert_eq!(history.entries.len(), 2);

        assert!(history.step(&app, false).is_some());
        app.set_active_pane(Pane::Tvs);
        assert!(history.navigate(
            &app,
            Pane::Profiles,
            NavSel::Profile(ProfileKind::System, SharedString::from("profile-1")),
        ));
        assert_eq!(history.entries.len(), 2);
        assert!(history.step(&app, true).is_none());
    }
}
