//! Desktop notifications + the per-user settings service.
//!
//! The notifier funnels every emit through one worker task (root's
//! flap-storm hardening: at most one in-flight `show_async`, category
//! gate checked lock-free before enqueue). Settings persist to
//! `tray.toml` and are served on the user socket — a cached pub at
//! `user/tray/settings`, rpcs under `user/tray/rpc/*`, and a liveliness
//! token — so the GUI personalizes them per user, live.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use notify_rust::Notification;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tisvlay_wire::keys;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::query::Query;
use zenoh_ext::{AdvancedPublisherBuilderExt, CacheConfig, MissDetectionConfig};

/// Wire/config body for the notification switches — same shape the
/// GUI's settings pane speaks (field names are the category keys).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationSettings {
    pub tv_connection: bool,
    pub tv_pairing: bool,
    pub tv_wake: bool,
    pub daemon_offline: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        // Every category documented default-on (root parity).
        Self {
            tv_connection: true,
            tv_pairing: true,
            tv_wake: true,
            daemon_offline: true,
        }
    }
}

/// Categories as bit positions in the notifier's atomic gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    TvConnection = 0,
    TvPairing = 1,
    TvWake = 2,
    DaemonOffline = 3,
}

impl NotificationSettings {
    fn bits(self) -> u8 {
        (self.tv_connection as u8)
            | (self.tv_pairing as u8) << 1
            | (self.tv_wake as u8) << 2
            | (self.daemon_offline as u8) << 3
    }

    /// Merge a partial wire update (`null`/absent fields keep current).
    fn merged_with(mut self, patch: &Value) -> Self {
        let apply = |field: &str, slot: &mut bool| {
            if let Some(value) = patch.get(field).and_then(Value::as_bool) {
                *slot = value;
            }
        };
        apply("tv_connection", &mut self.tv_connection);
        apply("tv_pairing", &mut self.tv_pairing);
        apply("tv_wake", &mut self.tv_wake);
        apply("daemon_offline", &mut self.daemon_offline);
        self
    }
}

/// One queued toast.
#[derive(Debug)]
struct Emit {
    summary: String,
    body: String,
    icon: &'static str,
}

/// Lock-free category gate + single-worker emit queue.
#[derive(Clone)]
pub struct Notifier {
    enabled: Arc<AtomicU8>,
    tx: mpsc::UnboundedSender<Emit>,
}

impl std::fmt::Debug for Notifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifier").finish_non_exhaustive()
    }
}

impl Notifier {
    pub fn new(initial: NotificationSettings) -> Self {
        let enabled = Arc::new(AtomicU8::new(initial.bits()));
        let (tx, mut rx) = mpsc::unbounded_channel::<Emit>();
        tokio::spawn(async move {
            while let Some(emit) = rx.recv().await {
                // One in-flight show at a time: a flap storm while the
                // notification daemon is absent collapses to sequential
                // short failures instead of N concurrent stalls.
                let result = Notification::new()
                    .appname("TisVlay")
                    .summary(&emit.summary)
                    .body(&emit.body)
                    .icon(emit.icon)
                    .show_async()
                    .await;
                if let Err(e) = result {
                    debug!(%e, summary = %emit.summary, "notification emit failed");
                }
            }
        });
        Self { enabled, tx }
    }

    pub fn set_enabled(&self, settings: NotificationSettings) {
        self.enabled.store(settings.bits(), Ordering::Release);
    }

    pub fn notify(&self, category: Category, summary: &str, body: &str, icon: &'static str) {
        if self.enabled.load(Ordering::Acquire) & (1 << category as u8) == 0 {
            return;
        }
        let _ = self.tx.send(Emit {
            summary: summary.to_string(),
            body: body.to_string(),
            icon,
        });
    }

    /// Bypasses the category gate — the settings pane's test button.
    fn notify_test(&self) {
        let _ = self.tx.send(Emit {
            summary: "Test notification".into(),
            body: "TisVlay tray notifications are working.".into(),
            icon: "video-display",
        });
    }
}

/// Default `tray.toml` location.
pub fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "tisvlay")
        .map(|dirs| dirs.config_dir().join("tray.toml"))
        .unwrap_or_else(|| PathBuf::from("tisvlay-tray.toml"))
}

pub fn load_settings(path: &Path) -> Result<NotificationSettings> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(NotificationSettings::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn save_settings(path: &Path, settings: &NotificationSettings) {
    let Ok(text) = toml::to_string_pretty(settings) else {
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!(%e, "tray config dir create failed");
        return;
    }
    let tmp = path.with_extension("toml.tmp");
    if std::fs::write(&tmp, text).is_ok()
        && let Err(e) = std::fs::rename(&tmp, path)
    {
        warn!(%e, "tray config write failed");
    }
}

/// Serve the settings on the user socket: cached pub + rpcs +
/// liveliness. Runs for the process lifetime; needs no ksni, so tests
/// drive it headlessly.
pub async fn serve_settings(
    user: Session,
    site: String,
    config_path: PathBuf,
    notifier: Notifier,
) -> Result<()> {
    let mut settings = load_settings(&config_path).unwrap_or_default();
    notifier.set_enabled(settings);

    let publisher = user
        .declare_publisher(keys::user_tray_settings(&site))
        .encoding(Encoding::APPLICATION_JSON)
        .cache(CacheConfig::default().max_samples(1))
        .sample_miss_detection(MissDetectionConfig::default().heartbeat(Duration::from_millis(500)))
        .publisher_detection()
        .await
        .map_err(|e| anyhow!("declare tray settings publisher: {e}"))?;
    let publish = |settings: NotificationSettings| {
        let publisher = &publisher;
        async move {
            let bytes = serde_json::to_vec(&settings).unwrap_or_default();
            if let Err(e) = publisher.put(bytes).await {
                warn!(%e, "tray settings publish failed");
            }
        }
    };
    publish(settings).await;

    let (query_tx, mut queries) = mpsc::unbounded_channel::<Query>();
    let _queryable = user
        .declare_queryable(keys::user_tray_rpc_wild(&site))
        .complete(true)
        .callback(move |query| {
            let _ = query_tx.send(query);
        })
        .await
        .map_err(|e| anyhow!("declare tray rpc queryable: {e}"))?;

    // Liveliness after the rpc queryable: a client that observes the
    // tray alive must find the rpc surface already declared.
    let _alive = user
        .liveliness()
        .declare_token(keys::user_alive_tray(&site))
        .await
        .map_err(|e| anyhow!("declare tray liveliness: {e}"))?;

    info!(site, config = %config_path.display(), "tray settings service running");
    while let Some(query) = queries.recv().await {
        let verb = keys::parse_user_tray_rpc(&site, query.key_expr().as_str())
            .unwrap_or_default()
            .to_string();
        let body: Value = query
            .payload()
            .and_then(|bytes| serde_json::from_slice(&bytes.to_bytes()).ok())
            .unwrap_or(Value::Null);
        match verb.as_str() {
            "get" => reply_json(&query, &serde_json::to_value(settings).unwrap_or_default()).await,
            "update" => {
                settings = settings.merged_with(&body);
                notifier.set_enabled(settings);
                save_settings(&config_path, &settings);
                publish(settings).await;
                reply_json(&query, &serde_json::to_value(settings).unwrap_or_default()).await;
            }
            "test" => {
                notifier.notify_test();
                reply_json(&query, &json!({})).await;
            }
            "info" => {
                reply_json(
                    &query,
                    &json!({
                        "version": env!("CARGO_PKG_VERSION"),
                        "state_path": config_path.display().to_string(),
                    }),
                )
                .await;
            }
            other => {
                let error = tisvlay_wire::error::WireError::bad_request(format!(
                    "no such tray rpc: {other}"
                ));
                let bytes = serde_json::to_vec(&error).unwrap_or_default();
                let _ = query
                    .reply_err(bytes)
                    .encoding(Encoding::APPLICATION_JSON)
                    .await;
            }
        }
    }
    Ok(())
}

async fn reply_json(query: &Query, payload: &Value) {
    let bytes = serde_json::to_vec(payload).unwrap_or_default();
    let key = query.key_expr().clone().into_owned();
    if let Err(e) = query
        .reply(key, bytes)
        .encoding(Encoding::APPLICATION_JSON)
        .await
    {
        warn!(%e, "tray rpc reply failed");
    }
}
