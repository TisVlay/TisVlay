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

use crate::compat::{DaemonClient, SessionClient};
use slint::{ComponentHandle, Model, SharedString};
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
    let apply: ApplyFn = {
        let daemon = daemon.clone();
        let session = session.clone();
        let editor = editor.clone();
        Rc::new(move |app: &AppWindow, pane: Pane, sel: &NavSel| {
            app.set_active_pane(pane);
            match sel {
                NavSel::Tv(uuid) => app.set_selected_tv_index(resolve_tv_index(app, uuid)),
                NavSel::Profile(kind, uuid) if !uuid.is_empty() => {
                    select_profile_in_ui(app, &daemon, &session, &editor, *kind, uuid.clone());
                }
                _ => {}
            }
        })
    };

    // Record a new destination: snapshot the outgoing pane's live
    // selection, drop any forward history, push, apply.
    let go: GoFn = {
        let history = history.clone();
        let apply = apply.clone();
        Rc::new(move |app: &AppWindow, pane: Pane, sel: NavSel| {
            {
                let mut h = history.borrow_mut();
                let cur = h.cursor;
                let cur_pane = h.entries[cur].pane;
                h.entries[cur].sel = read_current_sel(app, cur_pane);
                // Re-selecting the active pane + selection (e.g. clicking
                // the current sidebar item) shouldn't grow the history.
                if h.entries[cur].pane == pane && h.entries[cur].sel == sel {
                    return;
                }
                h.entries.truncate(cur + 1);
                h.entries.push(NavEntry {
                    pane,
                    sel: sel.clone(),
                });
                h.cursor = h.entries.len() - 1;
            }
            apply(app, pane, &sel);
        })
    };

    // Move one step back (`forward == false`) or forward through the
    // history, capturing the page being left so its selection is current.
    let step: StepFn = {
        let history = history.clone();
        let apply = apply.clone();
        Rc::new(move |app: &AppWindow, forward: bool| {
            let dest = {
                let mut h = history.borrow_mut();
                let cur = h.cursor;
                let next = if forward {
                    if cur + 1 >= h.entries.len() {
                        return;
                    }
                    cur + 1
                } else {
                    if cur == 0 {
                        return;
                    }
                    cur - 1
                };
                let cur_pane = h.entries[cur].pane;
                h.entries[cur].sel = read_current_sel(app, cur_pane);
                h.cursor = next;
                (h.entries[next].pane, h.entries[next].sel.clone())
            };
            apply(app, dest.0, &dest.1);
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
