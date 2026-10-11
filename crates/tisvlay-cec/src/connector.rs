//! `CecConnector` — factory, wake, and (trivial) pairing for CEC TVs.
//!
//! The connector owns a [`CecLinkOpener`] so tests can swap a real
//! `/dev/cecN` open for a scripted fake. Its defining quirk vs. a network
//! backend: the bus reaches the TV even in standby, so `connect` probes
//! power only to confirm the TV answers on the bus but *accepts* a standby
//! TV (`reachable_in_standby` — Connected ≠ power). The daemon's `power_on`
//! then routes to the wake path by the reported power state, not by whether
//! connect succeeds.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use linux_cec::operand::PowerStatus;
use tisvlay_core::{
    Credentials, Error, PairingSession, PairingState, Result, Transport, TvConnection, TvConnector,
    VendorConfig,
};
use tracing::instrument;

use crate::config::{CecVendorConfig, DEFAULT_OSD_NAME};
use crate::connection::CecConnection;
use crate::link::{CecLinkOpener, LinuxCecLinkOpener};

/// Delay between post-wake power-status probes, and how many to try before
/// giving up. A CEC TV can take a second or more to report `On` after
/// `Image View On`, so this is deliberately more patient than a network
/// backend's verify loop.
const WAKE_VERIFY_DELAY: Duration = Duration::from_millis(1500);
const WAKE_VERIFY_ATTEMPTS: usize = 4;

/// Factory for CEC TV connections.
#[derive(Debug)]
pub struct CecConnector {
    opener: Arc<dyn CecLinkOpener>,
}

impl CecConnector {
    /// Create a connector that opens real `/dev/cecN` devices.
    pub fn new() -> Self {
        Self {
            opener: Arc::new(LinuxCecLinkOpener),
        }
    }

    /// Construct with a custom link opener (tests inject a fake).
    #[cfg(test)]
    pub(crate) fn with_opener(opener: Arc<dyn CecLinkOpener>) -> Self {
        Self { opener }
    }

    /// Construct with an injected fake link opener — the out-of-crate
    /// test seam (`test-link` feature).
    #[cfg(any(test, feature = "test-link"))]
    pub fn with_fake_opener(opener: crate::link::FakeCecLinkOpener) -> Self {
        Self {
            opener: Arc::new(opener),
        }
    }
}

impl Default for CecConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a CEC power status means the TV is reachable for control —
/// i.e. on, or transitioning on.
fn reachable(status: PowerStatus) -> bool {
    matches!(status, PowerStatus::On | PowerStatus::ToOn)
}

/// The OSD name a connection-holding open advertises: the configured name
/// when advertising is on, nothing when it's off. (The bare wake and the
/// discovery probe never advertise — they pass `None` directly.)
fn advertised_name(config: &CecVendorConfig) -> Option<&str> {
    config
        .advertise_osd_name
        .then_some(config.osd_name.as_str())
}

#[async_trait]
impl TvConnector for CecConnector {
    fn transport(&self) -> Transport {
        Transport::Cec
    }

    fn reachable_in_standby(&self, _vendor_config: &VendorConfig) -> bool {
        // The CEC bus stays live in standby, so a standby TV is still
        // connected (Power ≠ Connected). `power_on` wakes it by power state,
        // not by connect success.
        true
    }

    fn emulate_screen_via_power(&self, vendor_config: &VendorConfig) -> bool {
        // HDMI-CEC has no discrete screen-blank, so honor screen actions by
        // powering the TV (default on). The daemon reads this to advertise
        // SCREEN and dispatch screen_off/screen_on as power_off/power_on.
        vendor_config
            .parse::<CecVendorConfig>(crate::VENDOR_ID)
            .map(|c| c.emulate_screen_via_power)
            .unwrap_or(false)
    }

    #[instrument(skip(self, vendor_config))]
    async fn connect(&self, vendor_config: &VendorConfig) -> Result<Box<dyn TvConnection>> {
        let config = vendor_config.parse::<CecVendorConfig>(crate::VENDOR_ID)?;
        let link = self
            .opener
            .open(
                &config.device,
                config.physical_address.to_linux(),
                advertised_name(&config),
            )
            .await?;
        // Probe power to confirm the TV actually answers on the bus — a
        // powered-off-at-the-wall TV (or one on a different HDMI input)
        // drops off CEC and this query fails, so `connect` fails and the TV
        // reads disconnected. We do NOT refuse standby, though: the bus is
        // reachable in standby, so a standby TV is legitimately connected;
        // the daemon's `power_on` wakes it based on the reported power state.
        let _status = link.query_power().await?;
        // Query the TV's identity once here (bounded, best-effort) so the
        // connection can return it synchronously via `hardware_id`/`brand`.
        let identity = crate::identity::query(link.as_ref()).await;
        Ok(Box::new(CecConnection::new(
            link,
            self.opener.clone(),
            config.device,
            identity,
        )))
    }

    fn supports_wake(&self, _vendor_config: &VendorConfig) -> bool {
        // The adapter is on the bus even when the TV sleeps, so we can
        // always send Image View On — wake is supported for any
        // configured device.
        true
    }

    #[instrument(skip(self, vendor_config))]
    async fn wake(&self, vendor_config: &VendorConfig) -> Result<()> {
        let config = vendor_config.parse::<CecVendorConfig>(crate::VENDOR_ID)?;
        let link = self
            .opener
            .open(&config.device, config.physical_address.to_linux(), None)
            .await?;
        link.power_on().await
    }

    #[instrument(skip(self, vendor_config))]
    async fn wake_and_connect(
        &self,
        vendor_config: &VendorConfig,
    ) -> Result<Box<dyn TvConnection>> {
        let config = vendor_config.parse::<CecVendorConfig>(crate::VENDOR_ID)?;
        let mut link = self
            .opener
            .open(
                &config.device,
                config.physical_address.to_linux(),
                advertised_name(&config),
            )
            .await?;
        // Image View On is fire-and-forget, a freshly-opened adapter is
        // still claiming its logical address, and a just-standby TV can
        // ignore the first wake — so a single send is easily lost. Re-send
        // on every attempt (One Touch Play is idempotent for an already-on
        // TV) and poll power between sends until the TV reports on.
        // A permission failure (the daemon can't open the node) surfaces
        // from the initial `open()?` above, before this loop runs — so the
        // loop only handles the wake itself.
        for _ in 0..WAKE_VERIFY_ATTEMPTS {
            match link.power_on().await {
                Ok(()) => {}
                // A dead adapter won't recover by retrying — surface the
                // concrete error now instead of burning the whole verify
                // window on a generic "did not report power-on".
                Err(e @ Error::ConnectionClosed) => return Err(e),
                // Otherwise the send may have failed on a lost
                // logical-address claim; re-open to re-claim before the
                // next re-send (keep the old handle if the re-open fails).
                Err(_) => {
                    if let Ok(fresh) = self
                        .opener
                        .open(
                            &config.device,
                            config.physical_address.to_linux(),
                            advertised_name(&config),
                        )
                        .await
                    {
                        link = fresh;
                    }
                }
            }
            tokio::time::sleep(WAKE_VERIFY_DELAY).await;
            if let Ok(status) = link.query_power().await
                && reachable(status)
            {
                let identity = crate::identity::query(link.as_ref()).await;
                return Ok(Box::new(CecConnection::new(
                    link,
                    self.opener.clone(),
                    config.device.clone(),
                    identity,
                )));
            }
        }
        Err(Error::backend(
            crate::VENDOR_ID,
            "TV did not report power-on after repeated Image View On",
        ))
    }

    #[instrument(skip(self, vendor_config))]
    fn start_pairing(&self, vendor_config: &VendorConfig) -> Result<PairingSession> {
        // CEC has no authentication — any device on the bus is reachable.
        // Validate the config parses, then complete immediately with empty
        // credentials (the trivial end of the pairing state machine).
        let _config = vendor_config.parse::<CecVendorConfig>(crate::VENDOR_ID)?;
        let (session, _command_rx, state_tx) = PairingSession::create();
        tokio::spawn(async move {
            let _ = state_tx.send(PairingState::complete(Credentials::new("")));
        });
        Ok(session)
    }

    fn build_config_from_pairing(
        &self,
        pairing_params: &VendorConfig,
        _credentials: &Credentials,
    ) -> Result<VendorConfig> {
        // No credentials to merge — echo the validated config back so the
        // daemon persists a well-formed `[tv.cec]` entry.
        let config = pairing_params.parse::<CecVendorConfig>(crate::VENDOR_ID)?;
        VendorConfig::from_typed(&config)
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("serialize config: {e}")))
    }

    fn config_schema(&self) -> Vec<tisvlay_core::PairingField> {
        use tisvlay_core::{PairingField, PairingFieldType};
        vec![
            PairingField {
                name: "device".into(),
                label: "CEC device".into(),
                helper: "Path to the kernel CEC character device (e.g. /dev/cec0).".into(),
                field_type: PairingFieldType::Path,
                required: true,
                placeholder: "/dev/cec0".into(),
                // Pre-filled from the device picker: discovery emits
                // `vendor_data["device"]` for each /dev/cecN node.
                autofill: "discovery.device".into(),
                ..Default::default()
            },
            PairingField {
                name: "physical_address".into(),
                label: "Physical address".into(),
                helper: "CEC address the adapter claims, matching the TV input it is plugged \
                         into (1.0.0.0 = HDMI 1). Leave as the default for GPU DisplayPort-CEC."
                    .into(),
                field_type: PairingFieldType::String,
                required: false,
                placeholder: "1.0.0.0".into(),
                default: "1.0.0.0".into(),
                ..Default::default()
            },
            PairingField {
                name: "emulate_screen_via_power".into(),
                label: "Screen is Power".into(),
                helper: "Fulfill screen-blank actions by powering the TV. HDMI-CEC can't blank \
                         the panel while leaving the TV on, so with this on a screen-off action \
                         (e.g. the default idle/lock/DPMS profile) powers the TV off and \
                         screen-on powers it back on. Turn off to have this device ignore screen \
                         actions instead."
                    .into(),
                field_type: PairingFieldType::Bool,
                required: false,
                default: "true".into(),
                ..Default::default()
            },
            PairingField {
                name: "osd_name".into(),
                label: "Input name".into(),
                helper: "Name this PC advertises to the TV — the label the TV shows for its \
                         HDMI input, the way a Chromecast names its input. Up to 14 \
                         characters; longer names are trimmed. Clearing the field restores \
                         the default."
                    .into(),
                field_type: PairingFieldType::String,
                required: false,
                placeholder: "HTPC".into(),
                default: DEFAULT_OSD_NAME.into(),
                ..Default::default()
            },
            PairingField {
                name: "advertise_osd_name".into(),
                label: "Advertise input name".into(),
                helper: "Tell the TV what to call this input. Turn off to advertise no name \
                         and keep the TV's own label for the input."
                    .into(),
                field_type: PairingFieldType::Bool,
                required: false,
                default: "true".into(),
                ..Default::default()
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::FakeCecLinkOpener;

    fn cfg() -> VendorConfig {
        VendorConfig::from_typed(&CecVendorConfig::default()).unwrap()
    }

    fn connector(initial: PowerStatus) -> CecConnector {
        CecConnector::with_opener(Arc::new(FakeCecLinkOpener::new(initial)))
    }

    #[test]
    fn transport_is_cec() {
        assert_eq!(connector(PowerStatus::On).transport(), Transport::Cec);
        assert!(
            !connector(PowerStatus::On)
                .transport()
                .is_network_dependent()
        );
    }

    #[tokio::test]
    async fn connect_succeeds_when_tv_is_on() {
        let conn = connector(PowerStatus::On).connect(&cfg()).await;
        assert!(conn.is_ok(), "connect should succeed when TV reports On");
        assert!(conn.unwrap().is_connected());
    }

    #[tokio::test]
    async fn connect_succeeds_when_tv_in_standby() {
        // The CEC bus stays reachable in standby, so a standby TV is
        // legitimately connected (Power ≠ Connected). The daemon wakes it by
        // power state, not by refusing the connection.
        let conn = connector(PowerStatus::Standby).connect(&cfg()).await;
        assert!(conn.is_ok(), "connect succeeds for a standby (on-bus) TV");
        assert!(conn.unwrap().is_connected());
    }

    #[tokio::test]
    async fn connect_surfaces_permission_denied() {
        // A node the daemon can enumerate but not open must surface the
        // typed permission error (not a generic backend/"CEC error"), so
        // the pairing/connect path can carry an actionable reason.
        let conn = CecConnector::with_opener(Arc::new(FakeCecLinkOpener::permission_denied()))
            .connect(&cfg())
            .await;
        assert!(
            matches!(conn, Err(Error::PermissionDenied { .. })),
            "expected PermissionDenied on an unopenable node",
        );
    }

    #[test]
    fn reachable_in_standby_is_true() {
        assert!(connector(PowerStatus::On).reachable_in_standby(&cfg()));
    }

    #[test]
    fn display_config_shows_defaulted_emulate_screen_via_power() {
        // A config that omits `emulate_screen_via_power` (a TV paired before
        // the field existed, so it rides the serde default) must still show
        // the field in the read-only view — falling back to the schema
        // default, matching what the edit form prefills. Regression for the
        // "option only visible in edit mode" GUI bug.
        let cfg: VendorConfig = toml::from_str(r#"device = "/dev/cec0""#).unwrap();
        let rows = connector(PowerStatus::On).display_config(&cfg);
        // All five `UserInput` fields show even though only `device` is
        // stored — the others fall back to their schema defaults (osd_name to
        // the default project name). Assert on the effective value, not the
        // (tweakable) label copy.
        assert_eq!(rows.len(), 5, "every UserInput field shows: {rows:?}");
        assert!(
            rows.iter().any(|(_, value)| value == "true"),
            "defaulted emulate_screen_via_power shows its effective value: {rows:?}",
        );
    }

    #[test]
    fn emulate_screen_via_power_defaults_on_and_honors_opt_out() {
        // Default config → emulate on (stock screen profiles power the TV).
        assert!(connector(PowerStatus::On).emulate_screen_via_power(&cfg()));
        // Opt-out config → off (device ignores screen actions).
        let off = VendorConfig::from_typed(&CecVendorConfig {
            emulate_screen_via_power: false,
            ..Default::default()
        })
        .unwrap();
        assert!(!connector(PowerStatus::On).emulate_screen_via_power(&off));
    }

    #[tokio::test(start_paused = true)]
    async fn wake_and_connect_powers_on_then_connects() {
        // Fake starts in standby; power_on flips it to On, and the verify
        // loop then sees it reachable. `start_paused` makes the verify
        // sleeps run in virtual time.
        let conn = connector(PowerStatus::Standby)
            .wake_and_connect(&cfg())
            .await;
        assert!(conn.is_ok(), "wake_and_connect should connect after wake");
        assert!(conn.unwrap().is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn wake_and_connect_resends_until_tv_wakes() {
        // The TV drops the first two Image View On sends and only wakes on
        // the third. A single fire-and-forget send would strand it in
        // standby; the re-send loop must keep trying across verify attempts.
        let conn = CecConnector::with_opener(Arc::new(FakeCecLinkOpener::new_flaky_wake(2)))
            .wake_and_connect(&cfg())
            .await;
        assert!(
            conn.is_ok(),
            "wake_and_connect should re-send Image View On until the TV wakes",
        );
        assert!(conn.unwrap().is_connected());
    }

    #[test]
    fn supports_wake_is_always_true() {
        assert!(connector(PowerStatus::Standby).supports_wake(&cfg()));
    }

    #[tokio::test]
    async fn start_pairing_completes_with_empty_credentials() {
        let session = connector(PowerStatus::On)
            .start_pairing(&cfg())
            .expect("pairing session starts");
        let mut rx = session.subscribe();
        loop {
            if let PairingState::Complete { credentials } = rx.borrow_and_update().clone() {
                assert_eq!(
                    credentials.data.expose(),
                    "",
                    "CEC pairs with empty credentials"
                );
                return;
            }
            rx.changed()
                .await
                .expect("pairing state channel stays open");
        }
    }

    #[test]
    fn invalid_config_is_rejected() {
        let mut bad = VendorConfig::new();
        bad.insert("device", 7);
        assert!(bad.parse::<CecVendorConfig>(crate::VENDOR_ID).is_err());
    }

    #[test]
    fn config_schema_exposes_device_and_physical_address() {
        use tisvlay_core::PairingFieldType;
        let schema = connector(PowerStatus::On).config_schema();
        assert_eq!(schema.len(), 5);
        assert_eq!(schema[0].name, "device");
        assert_eq!(schema[0].field_type, PairingFieldType::Path);
        assert!(schema[0].required);
        assert_eq!(schema[1].name, "physical_address");
        assert!(!schema[1].required, "physical address has a sane default");
        assert_eq!(schema[1].default, "1.0.0.0");
        assert_eq!(schema[2].name, "emulate_screen_via_power");
        assert_eq!(schema[2].field_type, PairingFieldType::Bool);
        assert!(!schema[2].required, "screen-via-power defaults on");
        assert_eq!(schema[2].default, "true");
        assert_eq!(schema[3].name, "osd_name");
        assert_eq!(schema[3].field_type, PairingFieldType::String);
        assert!(!schema[3].required, "osd_name is optional");
        assert_eq!(
            schema[3].default, DEFAULT_OSD_NAME,
            "schema default rides the same const as the serde default",
        );
        assert_eq!(schema[4].name, "advertise_osd_name");
        assert_eq!(schema[4].field_type, PairingFieldType::Bool);
        assert!(!schema[4].required, "advertising defaults on");
        assert_eq!(schema[4].default, "true");
    }

    #[tokio::test]
    async fn connect_advertises_configured_osd_name() {
        let opener = Arc::new(FakeCecLinkOpener::new(PowerStatus::On));
        let conn = CecConnector::with_opener(opener.clone());
        let cfg = VendorConfig::from_typed(&CecVendorConfig {
            osd_name: "HTPC".into(),
            ..Default::default()
        })
        .unwrap();
        conn.connect(&cfg).await.expect("connect");
        assert_eq!(
            opener.opened_osd_names(),
            vec![Some("HTPC".to_string())],
            "connect advertises the configured OSD name",
        );
    }

    #[tokio::test]
    async fn connect_advertises_nothing_when_toggled_off() {
        // The off switch is the advertise flag — a first-class bool the
        // schema form can always transmit — not an emptied name.
        let opener = Arc::new(FakeCecLinkOpener::new(PowerStatus::On));
        let conn = CecConnector::with_opener(opener.clone());
        let cfg = VendorConfig::from_typed(&CecVendorConfig {
            advertise_osd_name: false,
            ..Default::default()
        })
        .unwrap();
        conn.connect(&cfg).await.expect("connect");
        assert_eq!(
            opener.opened_osd_names(),
            vec![None],
            "advertise off passes no name to the link",
        );
    }

    #[tokio::test]
    async fn connect_advertises_default_tisvlay_when_unset() {
        let opener = Arc::new(FakeCecLinkOpener::new(PowerStatus::On));
        let conn = CecConnector::with_opener(opener.clone());
        // A config that doesn't set osd_name rides the "TisVlay" default.
        let cfg: VendorConfig = toml::from_str(r#"device = "/dev/cec0""#).unwrap();
        conn.connect(&cfg).await.expect("connect");
        assert_eq!(
            opener.opened_osd_names(),
            vec![Some("TisVlay".to_string())],
            "an unset osd_name advertises the default project name",
        );
    }

    #[tokio::test]
    async fn wake_advertises_nothing_even_with_a_name_configured() {
        // The bare wake opens, fires Image View On, and drops — it releases
        // the logical address before the TV would poll, so it names nothing.
        let opener = Arc::new(FakeCecLinkOpener::new(PowerStatus::Standby));
        let conn = CecConnector::with_opener(opener.clone());
        let cfg = VendorConfig::from_typed(&CecVendorConfig {
            osd_name: "HTPC".into(),
            ..Default::default()
        })
        .unwrap();
        conn.wake(&cfg).await.expect("wake");
        assert_eq!(
            opener.opened_osd_names(),
            vec![None],
            "the bare wake advertises no name even when one is configured",
        );
    }

    #[test]
    fn display_config_shows_a_set_osd_name() {
        // A configured osd_name surfaces as a read-only row (parity with the
        // emulate_screen_via_power display regression); an unset one elides,
        // keeping the default view zero-delta.
        let cfg = VendorConfig::from_typed(&CecVendorConfig {
            osd_name: "HTPC".into(),
            ..Default::default()
        })
        .unwrap();
        let rows = connector(PowerStatus::On).display_config(&cfg);
        assert!(
            rows.iter().any(|(_, value)| value == "HTPC"),
            "a set osd_name shows in the read-only view: {rows:?}",
        );
    }
}
