//! Portal D-Bus monitor detector: the attributed `Portal` remote source.
//!
//! krdp and RustDesk drive their capture through the desktop portal
//! (`org.freedesktop.portal.ScreenCast` / `.RemoteDesktop`). A second
//! session-bus connection turned into a receive-only monitor
//! (`org.freedesktop.DBus.Monitoring.BecomeMonitor`) sees those calls on
//! the wire and resolves the caller to an executable for attribution —
//! the generic screencast signal (the PipeWire detector) carries no app
//! identity, so this is what lets a known remote app arm unconditionally.
//!
//! Arming edge: a `CreateSession` call is the earliest observable portal
//! signal (it precedes `Start` and any injected input), so the source
//! arms there and clears on the session's `Closed` signal. Sessions are
//! keyed by their handle path, so several concurrent portal sessions from
//! one app coexist.
//!
//! Two hard filters keep the false positives out:
//!
//! - Only `ScreenCast` / `RemoteDesktop` count. `org.freedesktop.portal
//!   .InputCapture` shares the same object path and also has a
//!   `CreateSession`, but an InputCapture session (lan-mouse holds a
//!   standing one while the user is physically present) is never a remote
//!   session — it is ignored unconditionally.
//! - Only an allow-listed caller exe reports a `Portal` source. A portal
//!   user outside the list still trips the unattributed screencast signal
//!   (covered by the PipeWire detector under `remote_screencast_arms`), so
//!   nothing is lost by declining to attribute it here.
//!
//! Attribution rides the caller's unique bus name: the monitor connection
//! is receive-only, so the name→PID lookup
//! (`GetConnectionUnixProcessID`) goes through the service's normal
//! session-bus connection, then `/proc/<pid>/exe` gives the basename.
//!
//! Bootstrap (a service restart mid-session): the live portal sessions
//! are enumerated by walking `/org/freedesktop/portal/desktop/session/*`
//! — the path segment encodes the owner's mangled unique name. Session
//! objects are interface-blind (an InputCapture session looks identical),
//! so a bootstrapped owner is reported only *provisionally*: the
//! aggregator admits it only while the screencast signal corroborates a
//! live capture. A fresh `CreateSession` seen on the wire supersedes the
//! provisional guess with a direct, unconditional attribution.
//!
//! Degradation: `BecomeMonitor` denied or the portal absent (headless CI,
//! hardened distros) warns once and idles with supervised retry — it
//! never blocks or kills the session service, and the screencast detector
//! remains the fallback.

use futures_util::StreamExt;
use tisvlay_wire::payload::RemoteSourceKind;
use tracing::debug;
use zbus::MatchRule;
use zbus::message::Type as MessageType;

use crate::detector::Backoff;
use crate::{RemoteClient, RemoteReport};

/// The portal object namespace the monitor scopes its match rules to —
/// everything the desktop portal serves hangs under here, including the
/// per-session objects the bootstrap walk enumerates.
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// The session-object subtree; a live portal session is an object under
/// `<PORTAL_SESSION_PATH>/<mangled-owner>/<token>`.
const PORTAL_SESSION_PATH: &str = "/org/freedesktop/portal/desktop/session";

const SCREENCAST_IFACE: &str = "org.freedesktop.portal.ScreenCast";
const REMOTE_DESKTOP_IFACE: &str = "org.freedesktop.portal.RemoteDesktop";
const SESSION_IFACE: &str = "org.freedesktop.portal.Session";
const CREATE_SESSION: &str = "CreateSession";
const CLOSED: &str = "Closed";

/// Executable basenames that mint an attributed `Portal` source. A portal
/// user outside this list is left to the unattributed screencast signal.
const ALLOW_LIST: &[&str] = &["krdpserver", "rustdesk", "sunshine"];

/// The parsed shape of one monitored portal message, reduced to the
/// header fields the detector acts on. Owned so the parser is a pure fn
/// over a value and unit-tested without a live bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortalMessage {
    pub ty: MsgType,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub path: Option<String>,
    /// The caller's unique bus name (`:1.42`), for attribution.
    pub sender: Option<String>,
}

/// The D-Bus message types the parser distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MsgType {
    MethodCall,
    Signal,
    Other,
}

/// What a monitored portal message means to the detector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PortalEvent {
    /// A `ScreenCast`/`RemoteDesktop` `CreateSession` call from `sender`
    /// (its unique bus name) — the earliest arming edge.
    Create { sender: String },
    /// A `Session.Closed` signal for the session object at `path` — the
    /// disarming edge, keyed by handle.
    Closed { path: String },
}

/// Classify one monitored portal message. Pure over the reduced
/// [`PortalMessage`]; returns `None` for anything the detector ignores
/// (crucially, InputCapture `CreateSession` and every other portal
/// method).
pub(crate) fn classify(msg: &PortalMessage) -> Option<PortalEvent> {
    match msg.ty {
        MsgType::MethodCall => {
            let iface = msg.interface.as_deref()?;
            // InputCapture shares the object path and also has a
            // CreateSession — it is deliberately excluded here.
            if iface != SCREENCAST_IFACE && iface != REMOTE_DESKTOP_IFACE {
                return None;
            }
            if msg.member.as_deref() != Some(CREATE_SESSION) {
                return None;
            }
            let sender = msg.sender.clone()?;
            Some(PortalEvent::Create { sender })
        }
        MsgType::Signal => {
            if msg.interface.as_deref() != Some(SESSION_IFACE)
                || msg.member.as_deref() != Some(CLOSED)
            {
                return None;
            }
            let path = msg.path.clone()?;
            Some(PortalEvent::Closed { path })
        }
        MsgType::Other => None,
    }
}

/// Whether an executable basename mints an attributed `Portal` source.
pub(crate) fn is_allow_listed(exe: &str) -> bool {
    ALLOW_LIST.contains(&exe)
}

/// The basename of a resolved `/proc/<pid>/exe` target, stripping any Nix
/// or systemd wrapper decoration (`.krdpserver-wrapped` → `krdpserver`)
/// so the allow-list matches the logical program name.
pub(crate) fn exe_basename(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().filter(|s| !s.is_empty())?;
    // Wrapped binaries (Nix `makeWrapper`, `.foo-wrapped`) carry a leading
    // dot and a `-wrapped` suffix around the real name.
    let core = name
        .strip_prefix('.')
        .and_then(|n| n.strip_suffix("-wrapped"))
        .unwrap_or(name);
    Some(core.to_string())
}

/// Recover the owner's unique bus name from a portal session object path.
///
/// The portal mangles the owner's unique name into the path segment under
/// `.../session/` — the leading `:` is dropped and each `.` becomes `_`
/// (`:1.56` → `1_56`). A unique name has exactly one `.`, so the reverse
/// restores the `:` and turns the first `_` back into a `.`. Returns
/// `None` for a path that is not a portal session object.
pub(crate) fn session_owner(path: &str) -> Option<String> {
    let rest = path.strip_prefix(PORTAL_SESSION_PATH)?.strip_prefix('/')?;
    // The segment right under `.../session/` is the mangled owner; a
    // token (and anything deeper) follows.
    let segment = rest.split('/').next().filter(|s| !s.is_empty())?;
    Some(format!(":{}", segment.replacen('_', ".", 1)))
}

/// Build the narrow match-rule set the monitor subscribes to: portal
/// method calls and signals, scoped to the portal object namespace. The
/// per-message interface/member filtering is done in [`classify`] (a
/// path-namespace rule cannot express the InputCapture exclusion).
fn match_rules() -> Vec<MatchRule<'static>> {
    // Two rules under the portal namespace — one for the CreateSession
    // calls, one for the Closed signals. Fallible builders, but the
    // literals are static and valid; a failure just drops that rule.
    let mut rules = Vec::new();
    if let Ok(rule) = MatchRule::builder()
        .msg_type(MessageType::MethodCall)
        .path_namespace(PORTAL_PATH)
        .and_then(|b| b.interface(SCREENCAST_IFACE))
        .and_then(|b| b.member(CREATE_SESSION))
        .map(|b| b.build().to_owned())
    {
        rules.push(rule);
    }
    if let Ok(rule) = MatchRule::builder()
        .msg_type(MessageType::MethodCall)
        .path_namespace(PORTAL_PATH)
        .and_then(|b| b.interface(REMOTE_DESKTOP_IFACE))
        .and_then(|b| b.member(CREATE_SESSION))
        .map(|b| b.build().to_owned())
    {
        rules.push(rule);
    }
    if let Ok(rule) = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .path_namespace(PORTAL_PATH)
        .and_then(|b| b.interface(SESSION_IFACE))
        .and_then(|b| b.member(CLOSED))
        .map(|b| b.build().to_owned())
    {
        rules.push(rule);
    }
    rules
}

/// Reduce a live zbus message to the [`PortalMessage`] the parser reads.
fn reduce(msg: &zbus::Message) -> PortalMessage {
    let header = msg.header();
    let ty = match msg.message_type() {
        MessageType::MethodCall => MsgType::MethodCall,
        MessageType::Signal => MsgType::Signal,
        _ => MsgType::Other,
    };
    PortalMessage {
        ty,
        interface: header.interface().map(|i| i.as_str().to_string()),
        member: header.member().map(|m| m.as_str().to_string()),
        path: header.path().map(|p| p.as_str().to_string()),
        sender: header.sender().map(|s| s.as_str().to_string()),
    }
}

/// Spawn the portal monitor detector. Reports flow through `client` into
/// the session core. The task supervises its own second session-bus
/// connection and never blocks the caller; `main_bus` is the service's
/// normal session-bus connection, used for the receive-only monitor's
/// out-of-band name→PID lookups and the bootstrap tree-walk.
pub fn spawn(main_bus: zbus::Connection, client: RemoteClient) {
    tokio::spawn(run(main_bus, client));
}

/// Supervised monitor loop: open a monitor connection, bootstrap the live
/// sessions, then pump portal messages until the connection ends; back
/// off and retry. Quiet after the first warning so a box without a portal
/// does not spam the log.
async fn run(main_bus: zbus::Connection, client: RemoteClient) {
    let mut tracker = PortalTracker::default();
    let mut backoff = Backoff::default();
    loop {
        match session(&main_bus, &client, &mut tracker).await {
            Ok(()) => {
                debug!("portal monitor: connection ended; reconnecting");
                backoff.reset();
            }
            Err(Fault::CoreGone) => return,
            Err(Fault::Bus(e)) => backoff.note_failure("portal monitor: session bus", e),
        }
        // Whatever we thought was live is stale after a reconnect; clear
        // it so nothing lingers armed, then re-bootstrap on the next pass.
        // (A panic in `session` would skip this and strand the tracker's
        // sources until a restart — a design-pending residual.)
        for report in tracker.clear() {
            if !client.report(report) {
                return;
            }
        }
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

/// Why a monitor session ended.
enum Fault {
    /// The bus could not be reached or the monitor could not be armed.
    Bus(zbus::Error),
    /// The session core dropped the report channel — stop supervising.
    CoreGone,
}

impl From<zbus::Error> for Fault {
    fn from(e: zbus::Error) -> Self {
        Fault::Bus(e)
    }
}

impl From<zbus::fdo::Error> for Fault {
    fn from(e: zbus::fdo::Error) -> Self {
        Fault::Bus(e.into())
    }
}

/// One monitor session: arm a fresh monitor connection, bootstrap, and
/// pump messages until the stream ends. Reports are emitted as edges are
/// classified; the tracker owns the live-session bookkeeping.
async fn session(
    main_bus: &zbus::Connection,
    client: &RemoteClient,
    tracker: &mut PortalTracker,
) -> Result<(), Fault> {
    // A dedicated connection becomes the monitor; the main connection
    // stays a normal session bus for the name→PID lookups.
    let monitor = zbus::Connection::session().await?;
    let proxy = zbus::fdo::MonitoringProxy::new(&monitor).await?;
    proxy.become_monitor(&match_rules(), 0).await?;

    // Bootstrap: enumerate the sessions already live, attributing each
    // allow-listed owner provisionally (the aggregator gates admission on
    // a corroborating screencast signal). A failed walk is non-fatal —
    // the live stream still catches new sessions.
    match bootstrap_owners(main_bus).await {
        Ok(owners) => {
            for owner in owners {
                if let Some(exe) = resolve_exe(main_bus, &owner).await
                    && is_allow_listed(&exe)
                {
                    emit(client, tracker.provisional(&owner, exe))?;
                }
            }
        }
        Err(e) => debug!(%e, "portal monitor: bootstrap walk failed"),
    }

    let mut stream = zbus::MessageStream::from(monitor);
    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { continue };
        let Some(event) = classify(&reduce(&msg)) else {
            continue;
        };
        match event {
            PortalEvent::Create { sender } => {
                let Some(exe) = resolve_exe(main_bus, &sender).await else {
                    continue;
                };
                if is_allow_listed(&exe) {
                    emit(client, tracker.opened(&sender, exe))?;
                } else {
                    debug!(%exe, "portal monitor: non-allow-listed portal user ignored");
                }
            }
            PortalEvent::Closed { path } => {
                if let Some(owner) = session_owner(&path) {
                    emit(client, tracker.closed(&owner))?;
                }
            }
        }
    }
    Ok(())
}

/// Forward reports to the core; a dead core loop stops the supervisor.
fn emit(client: &RemoteClient, reports: Vec<RemoteReport>) -> Result<(), Fault> {
    for report in reports {
        if !client.report(report) {
            return Err(Fault::CoreGone);
        }
    }
    Ok(())
}

/// Resolve a unique bus name to its process's executable basename via the
/// main connection (the monitor connection is receive-only).
async fn resolve_exe(bus: &zbus::Connection, unique_name: &str) -> Option<String> {
    let proxy = zbus::fdo::DBusProxy::new(bus).await.ok()?;
    let name = zbus::names::BusName::try_from(unique_name.to_string()).ok()?;
    let pid = proxy.get_connection_unix_process_id(name).await.ok()?;
    let target = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    exe_basename(&target.to_string_lossy())
}

/// Walk `/org/freedesktop/portal/desktop/session/*` and return the owner
/// unique names of the live session objects. Introspection on the portal
/// object gives the child node names (the mangled owners).
async fn bootstrap_owners(bus: &zbus::Connection) -> zbus::Result<Vec<String>> {
    let proxy = zbus::fdo::IntrospectableProxy::builder(bus)
        .destination("org.freedesktop.portal.Desktop")?
        .path(PORTAL_SESSION_PATH)?
        .build()
        .await?;
    let xml = proxy.introspect().await?;
    // The direct children of `.../session/` are the mangled owner
    // segments; recover each to a unique name. Dedup so one owner with
    // several sessions reports once.
    let mut owners: Vec<String> = introspect_children(&xml)
        .into_iter()
        .filter_map(|child| session_owner(&format!("{PORTAL_SESSION_PATH}/{child}")))
        .collect();
    owners.sort();
    owners.dedup();
    Ok(owners)
}

/// Extract child node names from an introspection XML document. A tiny
/// hand parse (`<node name="…"/>` elements under the root) — pulling a
/// full XML crate in for three attributes is not worth it.
pub(crate) fn introspect_children(xml: &str) -> Vec<String> {
    let mut children = Vec::new();
    let mut rest = xml;
    while let Some(idx) = rest.find("<node name=") {
        rest = &rest[idx + "<node name=".len()..];
        let bytes = rest.as_bytes();
        let Some(&quote) = bytes.first() else { break };
        if quote != b'"' && quote != b'\'' {
            continue;
        }
        rest = &rest[1..];
        if let Some(end) = rest.find(quote as char) {
            children.push(rest[..end].to_string());
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    children
}

/// Tracks the live portal sessions this detector has attributed, so the
/// aggregate `Portal` source per exe stays live while any of that owner's
/// sessions is open and clears only when the last closes.
///
/// A session is either **confirmed** (seen opening on the wire, direct
/// attribution, always arms) or **provisional** (bootstrapped, owner-only,
/// admitted by the aggregator only while a screencast corroborates). One
/// owner unique name can hold several concurrent sessions (krdp opens a
/// ScreenCast *and* a RemoteDesktop session), so each owner carries an
/// open-session count: an open increments it, a `Closed` decrements it,
/// and the source clears when the count reaches zero — closing the first
/// of two concurrent sessions must not disarm while the other still
/// captures. A confirmed open for an owner promotes it out of provisional.
#[derive(Debug, Default)]
pub(crate) struct PortalTracker {
    /// Owner unique name → its tracked state. One entry per owner; the
    /// portal serves all of an owner's session objects under one subtree.
    owners: std::collections::BTreeMap<String, OwnerState>,
}

/// The per-owner tracking state: the attributed exe, whether any of its
/// sessions was confirmed on the wire, and the count of live sessions.
#[derive(Debug)]
struct OwnerState {
    exe: String,
    confirmed: bool,
    /// Live sessions for this owner: a bootstrap guess counts as one, and
    /// each confirmed `CreateSession` on the wire adds one. The source
    /// clears when this reaches zero.
    open: usize,
}

impl PortalTracker {
    /// A confirmed `CreateSession` from `owner` running `exe`: report the
    /// exe's source as confirmed (non-provisional, always arms) and
    /// remember the owner so its `Closed` clears it.
    pub(crate) fn opened(&mut self, owner: &str, exe: String) -> Vec<RemoteReport> {
        self.record(owner, exe, true)
    }

    /// A bootstrapped `owner` running `exe`, attributed only
    /// provisionally (the aggregator admits it only under a corroborating
    /// screencast signal).
    pub(crate) fn provisional(&mut self, owner: &str, exe: String) -> Vec<RemoteReport> {
        self.record(owner, exe, false)
    }

    fn record(&mut self, owner: &str, exe: String, confirmed: bool) -> Vec<RemoteReport> {
        match self.owners.get_mut(owner) {
            // Already tracked with the same exe: count the new session and
            // re-report only on a provisional→confirmed promotion (it drops
            // the corroboration requirement). An additional session under
            // an already-live source needs no fresh report.
            Some(state) if state.exe == exe => {
                state.open += 1;
                if confirmed && !state.confirmed {
                    state.confirmed = true;
                    return vec![portal_report(exe, true, false)];
                }
                Vec::new()
            }
            // A different exe for a known owner: clear the prior source,
            // then report the new one starting a fresh count.
            Some(state) => {
                let old_exe = std::mem::replace(&mut state.exe, exe.clone());
                state.confirmed = confirmed;
                state.open = 1;
                vec![
                    portal_report(old_exe, false, false),
                    portal_report(exe, true, !confirmed),
                ]
            }
            // Brand new owner: track it and report the source.
            None => {
                self.owners.insert(
                    owner.to_string(),
                    OwnerState {
                        exe: exe.clone(),
                        confirmed,
                        open: 1,
                    },
                );
                vec![portal_report(exe, true, !confirmed)]
            }
        }
    }

    /// One of `owner`'s sessions closed: decrement its count and clear the
    /// source only when the last one closes.
    pub(crate) fn closed(&mut self, owner: &str) -> Vec<RemoteReport> {
        let Some(state) = self.owners.get_mut(owner) else {
            return Vec::new();
        };
        state.open = state.open.saturating_sub(1);
        if state.open > 0 {
            return Vec::new();
        }
        let exe = self.owners.remove(owner).expect("just borrowed").exe;
        vec![portal_report(exe, false, false)]
    }

    /// Tear down every tracked session (a monitor reconnect): clear each
    /// live source so nothing lingers armed across the gap.
    pub(crate) fn clear(&mut self) -> Vec<RemoteReport> {
        let reports = self
            .owners
            .values()
            .map(|state| portal_report(state.exe.clone(), false, false))
            .collect();
        self.owners.clear();
        reports
    }
}

/// A `Portal` source report attributed to `exe`. `provisional` marks a
/// bootstrapped guess the aggregator admits only under a corroborating
/// screencast signal; a confirmed report always arms.
fn portal_report(exe: String, active: bool, provisional: bool) -> RemoteReport {
    RemoteReport {
        kind: RemoteSourceKind::Portal,
        detail: exe,
        active,
        provisional,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(iface: &str, member: &str, sender: &str) -> PortalMessage {
        PortalMessage {
            ty: MsgType::MethodCall,
            interface: Some(iface.into()),
            member: Some(member.into()),
            path: Some(PORTAL_PATH.into()),
            sender: Some(sender.into()),
        }
    }

    fn signal(iface: &str, member: &str, path: &str) -> PortalMessage {
        PortalMessage {
            ty: MsgType::Signal,
            interface: Some(iface.into()),
            member: Some(member.into()),
            path: Some(path.into()),
            sender: Some(":1.10".into()),
        }
    }

    #[test]
    fn create_session_on_capture_interfaces_arms() {
        assert_eq!(
            classify(&call(SCREENCAST_IFACE, CREATE_SESSION, ":1.42")),
            Some(PortalEvent::Create {
                sender: ":1.42".into()
            })
        );
        assert_eq!(
            classify(&call(REMOTE_DESKTOP_IFACE, CREATE_SESSION, ":1.43")),
            Some(PortalEvent::Create {
                sender: ":1.43".into()
            })
        );
    }

    #[test]
    fn input_capture_create_session_is_ignored() {
        // lan-mouse holds a standing InputCapture session while the user
        // is physically present — it shares the object path and the
        // CreateSession member, so only the interface filter excludes it.
        assert_eq!(
            classify(&call(
                "org.freedesktop.portal.InputCapture",
                CREATE_SESSION,
                ":1.56"
            )),
            None
        );
    }

    #[test]
    fn non_create_portal_methods_are_ignored() {
        // Start/SelectSources/etc. are not the arming edge.
        assert_eq!(classify(&call(SCREENCAST_IFACE, "Start", ":1.42")), None);
        assert_eq!(
            classify(&call(SCREENCAST_IFACE, "SelectSources", ":1.42")),
            None
        );
        assert_eq!(
            classify(&call(REMOTE_DESKTOP_IFACE, "ConnectToEIS", ":1.42")),
            None
        );
    }

    #[test]
    fn session_closed_signal_disarms() {
        let path = "/org/freedesktop/portal/desktop/session/1_42/ashpd_AbCd";
        assert_eq!(
            classify(&signal(SESSION_IFACE, CLOSED, path)),
            Some(PortalEvent::Closed { path: path.into() })
        );
        // A different signal on the session object is not a close.
        assert_eq!(classify(&signal(SESSION_IFACE, "Response", path)), None);
    }

    #[test]
    fn allow_list_matches_only_the_known_remote_apps() {
        assert!(is_allow_listed("krdpserver"));
        assert!(is_allow_listed("rustdesk"));
        assert!(is_allow_listed("sunshine"));
        assert!(!is_allow_listed("obs"));
        assert!(!is_allow_listed("firefox"));
        assert!(!is_allow_listed("lan-mouse"));
    }

    #[test]
    fn exe_basename_unwraps_nix_and_systemd_wrappers() {
        assert_eq!(
            exe_basename("/usr/bin/krdpserver").as_deref(),
            Some("krdpserver")
        );
        // Nix makeWrapper decorates the real binary as `.foo-wrapped`.
        assert_eq!(
            exe_basename("/nix/store/abc-krdp-1.0/bin/.krdpserver-wrapped").as_deref(),
            Some("krdpserver")
        );
        assert_eq!(
            exe_basename("/nix/store/xyz-lan-mouse/bin/.lan-mouse-wrapped").as_deref(),
            Some("lan-mouse")
        );
        // A plain wrapped-looking name without the dot prefix is left be.
        assert_eq!(
            exe_basename("/opt/thing-wrapped").as_deref(),
            Some("thing-wrapped")
        );
        assert_eq!(exe_basename("/").as_deref(), None);
        assert_eq!(exe_basename("").as_deref(), None);
    }

    #[test]
    fn session_owner_unmangles_the_path_segment() {
        // `:1.56` mangles to `1_56` in the path; the reverse restores it.
        assert_eq!(
            session_owner("/org/freedesktop/portal/desktop/session/1_56/ashpd_x").as_deref(),
            Some(":1.56")
        );
        // A three-plus-digit index still has exactly one dot restored.
        assert_eq!(
            session_owner("/org/freedesktop/portal/desktop/session/1_234/tok").as_deref(),
            Some(":1.234")
        );
        // The owner segment alone (no token) still resolves.
        assert_eq!(
            session_owner("/org/freedesktop/portal/desktop/session/1_9").as_deref(),
            Some(":1.9")
        );
        // A non-session path yields nothing.
        assert_eq!(
            session_owner("/org/freedesktop/portal/desktop").as_deref(),
            None
        );
        assert_eq!(session_owner("/some/other/path").as_deref(), None);
    }

    #[test]
    fn introspect_children_reads_node_names() {
        let xml = r#"<node>
            <node name="1_56"/>
            <node name='1_57'/>
            <interface name="org.freedesktop.DBus.Introspectable"/>
          </node>"#;
        // Only <node name=…> children, not the <interface> element.
        assert_eq!(introspect_children(xml), vec!["1_56", "1_57"]);
        assert!(introspect_children("<node></node>").is_empty());
    }

    #[test]
    fn tracker_confirmed_open_and_close_toggle_one_source() {
        let mut t = PortalTracker::default();
        let r = t.opened(":1.42", "krdpserver".into());
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert!(!r[0].provisional, "a confirmed open always arms");
        assert_eq!(r[0].detail, "krdpserver");
        // The single session closes: the source clears.
        let r = t.closed(":1.42");
        assert_eq!(r.len(), 1);
        assert!(!r[0].active);
        // Closing an unknown owner is a no-op.
        assert!(t.closed(":1.99").is_empty());
    }

    #[test]
    fn tracker_refcounts_concurrent_sessions_per_owner() {
        let mut t = PortalTracker::default();
        // krdp opens two capture sessions (ScreenCast + RemoteDesktop) from
        // one bus name: the first reports the source active, the second is
        // an already-armed no-op.
        let r = t.opened(":1.42", "krdpserver".into());
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert!(t.opened(":1.42", "krdpserver".into()).is_empty());
        // Closing the first handle must NOT clear the source — the other
        // session is still capturing.
        assert!(
            t.closed(":1.42").is_empty(),
            "one of two concurrent sessions closing keeps the source live"
        );
        // Closing the last handle clears it.
        let r = t.closed(":1.42");
        assert_eq!(r.len(), 1);
        assert!(!r[0].active);
        assert_eq!(r[0].detail, "krdpserver");
        // Fully drained: another close is a no-op.
        assert!(t.closed(":1.42").is_empty());
    }

    #[test]
    fn tracker_bootstrap_is_provisional_until_confirmed() {
        let mut t = PortalTracker::default();
        // A bootstrapped owner reports provisionally.
        let r = t.provisional(":1.42", "rustdesk".into());
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert!(
            r[0].provisional,
            "a bootstrapped source needs corroboration"
        );
        // A later CreateSession on the wire promotes it to confirmed —
        // re-reported as non-provisional so the aggregator drops the
        // corroboration requirement.
        let r = t.opened(":1.42", "rustdesk".into());
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert!(!r[0].provisional);
        // Already confirmed: another open is idempotent.
        assert!(t.opened(":1.42", "rustdesk".into()).is_empty());
    }

    #[test]
    fn tracker_clear_tears_down_every_live_source() {
        let mut t = PortalTracker::default();
        t.opened(":1.42", "krdpserver".into());
        t.provisional(":1.43", "rustdesk".into());
        let mut r = t.clear();
        r.sort_by(|a, b| a.detail.cmp(&b.detail));
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|report| !report.active));
        assert_eq!(r[0].detail, "krdpserver");
        assert_eq!(r[1].detail, "rustdesk");
        // A cleared tracker has nothing left to tear down.
        assert!(t.clear().is_empty());
    }
}
