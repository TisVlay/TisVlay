//! PowerDevil D-Bus forward path + reverse path.
//!
//! Forward: subscribe to `PropertiesChanged` on
//! `org.kde.ScreenBrightness.Display`, resolve the display's `Label`
//! to a TV via the shared monitor-name map, scale 0-max to 0-100, and
//! push a `BrightnessEvent` to the drain-loop.
//!
//! Reverse: call `org.kde.ScreenBrightness.Display.SetBrightness` on
//! the resolved display path. Scales 0-100 back to 0-max.
//!
//! Handles HDR correctly — KWin's Wayland brightness device is
//! bypassed in HDR mode, but PowerDevil still emits PropertiesChanged,
//! so this path covers both SDR and HDR.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, trace, warn};

use tisvlay_core::DaemonClient;

use crate::SessionState;
use crate::brightness::shared::{BrightnessEvent, BrightnessMechanism, Shared};

// ── Forward: PropertiesChanged observer ────────────────────────

/// Subscribe to `PropertiesChanged` on `org.kde.ScreenBrightness`
/// displays. Fires in both SDR and HDR modes. Publishes
/// `BrightnessEvent`s for any display whose `Label` resolves through
/// `shared.label_to_tv`.
pub async fn observe_kde_brightness_dbus(shared: Arc<Shared>, conn: Arc<zbus::Connection>) {
    use futures_util::StreamExt;

    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.kde.ScreenBrightness")
        .unwrap()
        .interface("org.freedesktop.DBus.Properties")
        .unwrap()
        .member("PropertiesChanged")
        .unwrap()
        .build();

    let Ok(mut stream) = zbus::MessageStream::for_match_rule(rule, &conn, None).await else {
        warn!("D-Bus brightness observer: failed to subscribe");
        return;
    };

    let mut path_cache: HashMap<String, (String, i32)> = HashMap::new();
    debug!("D-Bus brightness observer started");

    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { continue };

        let Ok((iface, changed, _)): Result<
            (
                String,
                HashMap<String, zbus::zvariant::OwnedValue>,
                Vec<String>,
            ),
            _,
        > = msg.body().deserialize() else {
            continue;
        };

        if iface != "org.kde.ScreenBrightness.Display" {
            continue;
        }
        let Some(val) = changed.get("Brightness") else {
            continue;
        };
        let Ok(brightness) = i32::try_from(val) else {
            continue;
        };

        let path = match msg.header().path() {
            Some(p) => p.to_string(),
            None => continue,
        };

        let (label, max) = match path_cache.get(&path) {
            Some(c) => c.clone(),
            None => {
                let l = read_display_prop::<String>(&conn, &path, "Label").await;
                let m = read_display_prop::<i32>(&conn, &path, "MaxBrightness").await;
                match (l, m) {
                    (Some(l), Some(m)) if m > 0 => {
                        path_cache.insert(path.clone(), (l.clone(), m));
                        (l, m)
                    }
                    _ => continue,
                }
            }
        };

        let tv_id = shared
            .label_to_tv
            .lock()
            .expect("poisoned")
            .get(&label)
            .cloned();
        if let Some(tv_id) = tv_id {
            shared
                .tv_display_info
                .lock()
                .expect("poisoned")
                .insert(tv_id.clone(), (path.clone(), max));

            // Suppress echo from our own reverse-path writes.
            {
                let mut sent = shared.last_reverse_sent.lock().expect("poisoned");
                if sent.get(&tv_id) == Some(&brightness) {
                    sent.remove(&tv_id);
                    continue;
                }
            }

            let level = ((brightness as f64 / max as f64) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u32;
            trace!(tv_id, brightness, max, level, "D-Bus brightness change");
            let _ = shared.event_tx.send(BrightnessEvent::Requested {
                tv_id,
                value: level,
            });
        }
    }
}

// ── Reverse: SetBrightness ─────────────────────────────────────

/// Push a TV backlight value to PowerDevil's D-Bus display via
/// `SetBrightness`. Scales 0-100 → 0-max.
pub async fn push_brightness_to_kde(
    shared: &Shared,
    conn: &zbus::Connection,
    tv_id: &str,
    backlight: u8,
) {
    let (path, max) = {
        let info = shared.tv_display_info.lock().expect("poisoned");
        match info.get(tv_id) {
            Some((p, m)) => (p.clone(), *m),
            None => return, // D-Bus observer hasn't resolved this TV yet
        }
    };

    let kde_value = ((backlight as f64 / 100.0) * max as f64)
        .round()
        .clamp(0.0, max as f64) as i32;

    shared
        .last_reverse_sent
        .lock()
        .expect("poisoned")
        .insert(tv_id.to_string(), kde_value);

    let result = conn
        .call_method(
            Some("org.kde.ScreenBrightness"),
            &*path,
            Some("org.kde.ScreenBrightness.Display"),
            "SetBrightness",
            &(kde_value, 0u32),
        )
        .await;

    match result {
        Ok(_) => trace!(
            tv_id,
            backlight, kde_value, "reverse: pushed brightness to PowerDevil"
        ),
        Err(e) => warn!(tv_id, error = %e, "reverse: SetBrightness failed"),
    }
}

async fn read_display_prop<T: TryFrom<zbus::zvariant::OwnedValue>>(
    conn: &zbus::Connection,
    path: &str,
    property: &'static str,
) -> Option<T> {
    let proxy = zbus::fdo::PropertiesProxy::builder(conn)
        .destination("org.kde.ScreenBrightness")
        .ok()?
        .path(path)
        .ok()?
        .build()
        .await
        .ok()?;
    let iface =
        zbus::names::InterfaceName::from_static_str_unchecked("org.kde.ScreenBrightness.Display");
    let prop = zbus::names::PropertyName::from_static_str_unchecked(property);
    T::try_from(proxy.get(iface, &prop).await.ok()?).ok()
}

// ── Reconcile (label→tv_id map only) ───────────────────────────

/// Rebuild `shared.label_to_tv` from EDID monitor names for any TV
/// whose profile opts into the D-Bus mechanism. The observer and
/// reverse path use this map to route D-Bus traffic.
///
/// Unlike `kde_wayland::reconcile`, there's no Wayland device
/// lifecycle — the D-Bus observer subscribes globally and filters by
/// TV id in the map.
pub async fn reconcile_label_map(
    shared: &Arc<Shared>,
    daemon: &DaemonClient,
    state: &Arc<RwLock<SessionState>>,
) {
    let tvs = match daemon.list_tvs().await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "list_tvs failed during dbus reconcile");
            return;
        }
    };

    let mut map: HashMap<String, String> = HashMap::new();
    {
        let s = state.read().await;
        for tv in &tvs {
            let Some(profile) = s.profiles.get(&tv.id) else {
                continue;
            };
            if !BrightnessMechanism::Dbus.accepts(profile.sync_backlight) {
                continue;
            }
            let Some(edid) = crate::brightness::shared::read_edid_for_tv(tv, &s) else {
                continue;
            };
            if let Some(name) = edid.monitor_name {
                map.insert(name, tv.id.clone());
            }
        }
    }

    let mut slot = shared.label_to_tv.lock().expect("poisoned");
    *slot = map;
}
