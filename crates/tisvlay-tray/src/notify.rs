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
use tisvlay_wire::endpoint::user_tray;
use tisvlay_wire::error::WireError;
use tisvlay_wire::{keys, rpc};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::query::Query;
use zenoh_ext::{AdvancedPublisherBuilderExt, CacheConfig, MissDetectionConfig};

pub use tisvlay_wire::rpc::NotificationSettings;

/// On-disk wrapper for the settings file: the schema version tag rides
/// the persisted form only, never the wire body. Absence *defines*
/// version 1; migration logic waits until a v2 exists.
#[derive(Serialize, Deserialize)]
struct PersistedSettings {
    #[serde(default = "schema_v1")]
    version: u32,
    #[serde(flatten)]
    settings: NotificationSettings,
}

fn schema_v1() -> u32 {
    1
}

/// Categories as bit positions in the notifier's atomic gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    TvConnection = 0,
    TvPairing = 1,
    TvWake = 2,
    DaemonOffline = 3,
}

/// The settings as the notifier's gate bits, one per [`Category`].
fn bits(settings: NotificationSettings) -> u8 {
    u8::from(settings.tv_connection)
        | u8::from(settings.tv_pairing) << 1
        | u8::from(settings.tv_wake) << 2
        | u8::from(settings.daemon_offline) << 3
}

/// One queued toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub summary: String,
    pub body: String,
    pub icon: &'static str,
}

/// Lock-free category gate + single-worker emit queue.
#[derive(Clone)]
pub struct Notifier {
    enabled: Arc<AtomicU8>,
    tx: mpsc::UnboundedSender<Toast>,
}

impl std::fmt::Debug for Notifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifier").finish_non_exhaustive()
    }
}

impl Notifier {
    /// A notifier that shows its toasts on the desktop (the session bus's
    /// notification daemon).
    pub fn new(initial: NotificationSettings) -> Self {
        let (notifier, mut rx) = Self::capturing(initial);
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
        notifier
    }

    /// A notifier whose toasts (category gate applied) arrive on the
    /// returned receiver instead of the desktop — what tests assert on,
    /// with no real notification popping up.
    pub fn capturing(initial: NotificationSettings) -> (Self, mpsc::UnboundedReceiver<Toast>) {
        let enabled = Arc::new(AtomicU8::new(bits(initial)));
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { enabled, tx }, rx)
    }

    pub fn set_enabled(&self, settings: NotificationSettings) {
        self.enabled.store(bits(settings), Ordering::Release);
    }

    pub fn notify(&self, category: Category, summary: &str, body: &str, icon: &'static str) {
        if self.enabled.load(Ordering::Acquire) & (1 << category as u8) == 0 {
            return;
        }
        let _ = self.tx.send(Toast {
            summary: summary.to_string(),
            body: body.to_string(),
            icon,
        });
    }

    /// Bypasses the category gate — the settings pane's test button.
    fn notify_test(&self) {
        let _ = self.tx.send(Toast {
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
        Ok(text) => toml::from_str::<PersistedSettings>(&text)
            .map(|p| p.settings)
            .with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(NotificationSettings::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// `tray.toml` as a reload sees it: a missing or unparsable file is
/// refused rather than read as the defaults.
fn read_for_reload(path: &Path) -> Result<NotificationSettings, WireError> {
    let text = std::fs::read_to_string(path).map_err(|e| WireError::store_unreadable(path, e))?;
    toml::from_str::<PersistedSettings>(&text)
        .map(|persisted| persisted.settings)
        .map_err(|e| WireError::store_unreadable(path, e))
}

/// Durably write `tray.toml`, keeping a hand edit's comments and layout
/// (`write_toml`), off the async runtime since the fsyncs block.
async fn save_settings(path: &Path, settings: &NotificationSettings) {
    let persisted = PersistedSettings {
        version: schema_v1(),
        settings: *settings,
    };
    let path = path.to_path_buf();
    let written =
        tokio::task::spawn_blocking(move || tisvlay_runtime::fs::write_toml(&path, &persisted))
            .await;
    match written {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => warn!(%e, "tray config write failed"),
        Err(e) => warn!(%e, "tray config write task failed"),
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
        .declare_queryable(user_tray::Incoming::wild(&site))
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
        let Some(((), incoming)) = user_tray::Incoming::receive(&site, query).await else {
            continue;
        };
        match incoming {
            user_tray::Incoming::Get((), responder) => responder.ok(&settings).await,
            user_tray::Incoming::Update(patch, responder) => {
                settings = settings.merged_with(&patch);
                notifier.set_enabled(settings);
                save_settings(&config_path, &settings).await;
                publish(settings).await;
                responder.ok(&settings).await;
            }
            user_tray::Incoming::Test((), responder) => {
                notifier.notify_test();
                responder.ok(&rpc::Ack {}).await;
            }
            user_tray::Incoming::Info((), responder) => {
                let info = rpc::ServiceInfo {
                    version: env!("CARGO_PKG_VERSION").into(),
                    state_path: config_path.display().to_string(),
                };
                responder.ok(&info).await;
            }
            user_tray::Incoming::Reload((), responder) => {
                // Every update is written before it's answered, so the
                // file only differs from `settings` after a hand edit,
                // which then applies whole. Unlike boot, a missing or
                // unparsable file is refused rather than defaulted.
                let reloaded = read_for_reload(&config_path).map(|disk| {
                    let changed = disk != settings;
                    if changed {
                        settings = disk;
                        notifier.set_enabled(settings);
                    }
                    changed
                });
                if reloaded == Ok(true) {
                    publish(settings).await;
                }
                responder
                    .answer(reloaded.map(|changed| rpc::ReloadReport { changed }))
                    .await;
            }
        }
    }
    Ok(())
}
