//! Portal D-Bus monitor detector: the attributed `Portal` stream source.
//!
//! krdp and RustDesk drive their capture through the desktop portal
//! (`org.freedesktop.portal.ScreenCast` / `.RemoteDesktop`). A second
//! session-bus connection turned into a receive-only monitor
//! (`org.freedesktop.DBus.Monitoring.BecomeMonitor`) sees those calls on
//! the wire and resolves the caller to an executable for attribution —
//! the generic screencast signal (the PipeWire detector) carries no app
//! identity, so this is what lets a known streaming app arm unconditionally.
//!
//! Arming edge: a `CreateSession` call is the earliest observable portal
//! signal (it precedes `Start` and any injected input), so the source
//! arms there. Sessions are keyed by their handle path, which the call
//! names up front (the caller's `session_handle_token` under its mangled
//! bus name), so several concurrent portal sessions from one app coexist.
//!
//! Disarming edges: the portal emits `Session.Closed` only when its own
//! side ends a session. A client's `Close` call or its exit tears the
//! session down silently, so the monitor also watches the owner's `Close`
//! calls and the bus driver's `NameOwnerChanged` for a vanished owner,
//! and a vanished portal clears every session it held.
//!
//! Two hard filters keep the false positives out:
//!
//! - Only `ScreenCast` / `RemoteDesktop` count. `org.freedesktop.portal
//!   .InputCapture` shares the same object path and also has a
//!   `CreateSession`, but an InputCapture session (lan-mouse holds a
//!   standing one while the user is physically present) is never a
//!   stream — it is ignored unconditionally.
//! - Only an allow-listed caller exe reports a `Portal` source. A portal
//!   user outside the list still trips the unattributed screencast signal
//!   (covered by the PipeWire detector under `screencast_arms`), so
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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use futures_util::StreamExt;
use tisvlay_protocol::payload::StreamSourceKind;
use tracing::debug;
use zbus::MatchRule;
use zbus::message::Type as MessageType;
use zbus::zvariant::Value;

use crate::detector::{Backoff, Fault};
use crate::{StreamClient, StreamReport};

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
/// The `CreateSession` option naming the session object's last path
/// segment. A call without it opens no session, so a live session always
/// has one.
const SESSION_HANDLE_TOKEN: &str = "session_handle_token";
const CLOSE: &str = "Close";
const CLOSED: &str = "Closed";
/// The portal's well-known name; its owner vanishing ends every session.
const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const BUS_NAME: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const NAME_OWNER_CHANGED: &str = "NameOwnerChanged";

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
    /// A `CreateSession` call's `session_handle_token` option.
    pub session_token: Option<String>,
    /// A `NameOwnerChanged` signal's name, when it lost its owner.
    pub vanished: Option<String>,
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
    /// (its unique bus name) for the session object at `session` — the
    /// earliest arming edge.
    Create { sender: String, session: String },
    /// The session object at `path` ended: the portal's `Closed` signal,
    /// or its owner's `Close` call.
    Closed { path: String },
    /// The client `owner` (a unique bus name) left the bus, taking every
    /// session it held with it.
    OwnerGone { owner: String },
    /// The portal itself left the bus, ending every session.
    PortalGone,
}

/// Classify one monitored portal message. Pure over the reduced
/// [`PortalMessage`]; returns `None` for anything the detector ignores
/// (crucially, InputCapture `CreateSession` and every other portal
/// method).
pub(crate) fn classify(msg: &PortalMessage) -> Option<PortalEvent> {
    let interface = msg.interface.as_deref()?;
    let member = msg.member.as_deref()?;
    match (msg.ty, interface, member) {
        // InputCapture shares the object path and also has a
        // CreateSession — it is deliberately excluded here.
        (MsgType::MethodCall, SCREENCAST_IFACE | REMOTE_DESKTOP_IFACE, CREATE_SESSION) => {
            let sender = msg.sender.clone()?;
            let token = msg.session_token.as_deref().filter(|t| is_valid_token(t))?;
            let session = format!("{PORTAL_SESSION_PATH}/{}/{token}", mangle(&sender)?);
            Some(PortalEvent::Create { sender, session })
        }
        // The portal refuses a Close from anyone but the owner.
        (MsgType::MethodCall, SESSION_IFACE, CLOSE) => {
            let path = msg.path.clone()?;
            (session_owner(&path) == msg.sender).then_some(PortalEvent::Closed { path })
        }
        (MsgType::Signal, SESSION_IFACE, CLOSED) => {
            let path = msg.path.clone()?;
            Some(PortalEvent::Closed { path })
        }
        (MsgType::Signal, BUS_NAME, NAME_OWNER_CHANGED) => match msg.vanished.as_deref()? {
            PORTAL_NAME => Some(PortalEvent::PortalGone),
            owner if owner.starts_with(':') => Some(PortalEvent::OwnerGone {
                owner: owner.to_owned(),
            }),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `token` is one the portal accepts as a handle token: a
/// non-empty run of ASCII letters, digits and underscores. A call with
/// any other token fails, so it never opens a session.
fn is_valid_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Mangle a unique bus name into its portal path segment: the leading
/// `:` is dropped and each `.` becomes `_` (`:1.56` → `1_56`). The
/// inverse of [`session_owner`]'s unmangling.
fn mangle(unique_name: &str) -> Option<String> {
    Some(unique_name.strip_prefix(':')?.replace('.', "_"))
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

/// Build the narrow match-rule set the monitor subscribes to: the portal
/// calls and signals scoped to the portal object namespace, plus the bus
/// driver's `NameOwnerChanged` for names losing their owner. The
/// per-message interface/member filtering is done in [`classify`] (a
/// path-namespace rule cannot express the InputCapture exclusion).
fn match_rules() -> Vec<MatchRule<'static>> {
    fn portal(
        ty: MessageType,
        interface: &'static str,
        member: &'static str,
    ) -> zbus::Result<MatchRule<'static>> {
        Ok(MatchRule::builder()
            .msg_type(ty)
            .path_namespace(PORTAL_PATH)?
            .interface(interface)?
            .member(member)?
            .build())
    }
    // An empty new owner (arg 2) means the name vanished; matching it here
    // keeps the bus's every other name change off the monitor.
    let vanished = || -> zbus::Result<MatchRule<'static>> {
        Ok(MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(BUS_NAME)?
            .path(BUS_PATH)?
            .interface(BUS_NAME)?
            .member(NAME_OWNER_CHANGED)?
            .arg(2, "")?
            .build())
    };
    // Fallible builders, but the literals are static and valid; a failure
    // just drops that rule.
    [
        portal(MessageType::MethodCall, SCREENCAST_IFACE, CREATE_SESSION),
        portal(
            MessageType::MethodCall,
            REMOTE_DESKTOP_IFACE,
            CREATE_SESSION,
        ),
        portal(MessageType::MethodCall, SESSION_IFACE, CLOSE),
        portal(MessageType::Signal, SESSION_IFACE, CLOSED),
        vanished(),
    ]
    .into_iter()
    .filter_map(Result::ok)
    .collect()
}

/// Reduce a live zbus message to the [`PortalMessage`] the parser reads.
/// Only the two bodies the parser needs are decoded: `CreateSession`'s
/// options and `NameOwnerChanged`'s name and new owner.
fn reduce(msg: &zbus::Message) -> PortalMessage {
    let header = msg.header();
    let ty = match msg.message_type() {
        MessageType::MethodCall => MsgType::MethodCall,
        MessageType::Signal => MsgType::Signal,
        _ => MsgType::Other,
    };
    let member = header.member().map(|m| m.as_str().to_string());
    let body = msg.body();
    let session_token = (member.as_deref() == Some(CREATE_SESSION))
        .then(|| body.deserialize::<(HashMap<&str, Value<'_>>,)>().ok())
        .flatten()
        .and_then(|(options,)| match options.get(SESSION_HANDLE_TOKEN)? {
            Value::Str(token) => Some(token.as_str().to_owned()),
            _ => None,
        });
    let vanished = (member.as_deref() == Some(NAME_OWNER_CHANGED))
        .then(|| body.deserialize::<(&str, &str, &str)>().ok())
        .flatten()
        .and_then(|(name, _, new_owner)| new_owner.is_empty().then(|| name.to_owned()));
    PortalMessage {
        ty,
        interface: header.interface().map(|i| i.as_str().to_string()),
        member,
        path: header.path().map(|p| p.as_str().to_string()),
        sender: header.sender().map(|s| s.as_str().to_string()),
        session_token,
        vanished,
    }
}

/// Spawn the portal monitor detector. Reports flow through `client` into
/// the session core. The task supervises its own second session-bus
/// connection and never blocks the caller; `main_bus` is the service's
/// normal session-bus connection, used for the receive-only monitor's
/// out-of-band name→PID lookups and the bootstrap tree-walk.
pub fn spawn(main_bus: zbus::Connection, client: StreamClient) {
    tokio::spawn(run(main_bus, client));
}

/// Supervised monitor loop: open a monitor connection, bootstrap the live
/// sessions, then pump portal messages until the connection ends; back
/// off and retry. Quiet after the first warning so a box without a portal
/// does not spam the log.
async fn run(main_bus: zbus::Connection, client: StreamClient) {
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

// The portal monitor also faults on fdo errors; the shared `Fault` (a
// crate-local type) can carry the conversion from this module.
impl From<zbus::fdo::Error> for Fault {
    fn from(e: zbus::fdo::Error) -> Self {
        Self::Bus(e.into())
    }
}

/// One monitor session: arm a fresh monitor connection, bootstrap, and
/// pump messages until the stream ends. Reports are emitted as edges are
/// classified; the tracker owns the live-session bookkeeping.
async fn session(
    main_bus: &zbus::Connection,
    client: &StreamClient,
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
    match bootstrap_sessions(main_bus).await {
        Ok(owners) => {
            for (owner, sessions) in owners {
                if let Some(exe) = resolve_exe(main_bus, &owner).await
                    && is_allow_listed(&exe)
                {
                    emit(client, tracker.provisional(&owner, exe, sessions))?;
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
        let reports = match event {
            PortalEvent::Create { sender, session } => {
                let Some(exe) = resolve_exe(main_bus, &sender).await else {
                    continue;
                };
                if !is_allow_listed(&exe) {
                    debug!(%exe, "portal monitor: non-allow-listed portal user ignored");
                    continue;
                }
                tracker.opened(&sender, exe, session)
            }
            PortalEvent::Closed { path } => tracker.closed(&path),
            PortalEvent::OwnerGone { owner } => tracker.gone(&owner),
            PortalEvent::PortalGone => tracker.clear(),
        };
        emit(client, reports)?;
    }
    Ok(())
}

/// Forward reports to the core; a dead core loop stops the supervisor.
fn emit(client: &StreamClient, reports: Vec<StreamReport>) -> Result<(), Fault> {
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

/// Walk `/org/freedesktop/portal/desktop/session/*/*` and return each live
/// session object's path, grouped by its owner's unique name. The direct
/// children of `.../session/` are the mangled owners; theirs are the
/// session tokens.
async fn bootstrap_sessions(bus: &zbus::Connection) -> zbus::Result<BTreeMap<String, Vec<String>>> {
    let mut owners = BTreeMap::new();
    for segment in introspect_children(&introspect(bus, PORTAL_SESSION_PATH).await?) {
        let owner_path = format!("{PORTAL_SESSION_PATH}/{segment}");
        let Some(owner) = session_owner(&owner_path) else {
            continue;
        };
        let sessions: Vec<String> = introspect_children(&introspect(bus, &owner_path).await?)
            .into_iter()
            .map(|token| format!("{owner_path}/{token}"))
            .collect();
        if !sessions.is_empty() {
            owners.insert(owner, sessions);
        }
    }
    Ok(owners)
}

/// The portal's introspection XML for the object at `path`.
async fn introspect(bus: &zbus::Connection, path: &str) -> zbus::Result<String> {
    let proxy = zbus::fdo::IntrospectableProxy::builder(bus)
        .destination(PORTAL_NAME)?
        .path(path.to_owned())?
        .build()
        .await?;
    Ok(proxy.introspect().await?)
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
/// attribution, always arms) or **provisional** (bootstrapped, admitted
/// by the aggregator only while a screencast corroborates). One owner
/// unique name can hold several concurrent sessions (krdp opens a
/// ScreenCast *and* a RemoteDesktop session), so each owner carries the
/// set of its live session paths: closing the first of two concurrent
/// sessions must not disarm while the other still captures, and a session
/// seen closing twice (the portal's `Closed` and the owner's `Close`)
/// counts once. A confirmed open for an owner promotes it out of
/// provisional.
#[derive(Debug, Default)]
pub(crate) struct PortalTracker {
    /// Owner unique name → its tracked state. One entry per owner; the
    /// portal serves all of an owner's session objects under one subtree.
    owners: BTreeMap<String, OwnerState>,
}

/// The per-owner tracking state: the attributed exe, whether any of its
/// sessions was confirmed on the wire, and its live session paths. The
/// entry goes when the last session does, so the set is never empty.
#[derive(Debug)]
struct OwnerState {
    exe: String,
    confirmed: bool,
    sessions: BTreeSet<String>,
}

impl PortalTracker {
    /// A confirmed `CreateSession` from `owner` running `exe` for the
    /// session at `session`: report the exe's source as confirmed
    /// (non-provisional, always arms).
    pub(crate) fn opened(
        &mut self,
        owner: &str,
        exe: String,
        session: String,
    ) -> Vec<StreamReport> {
        self.record(owner, exe, vec![session], true)
    }

    /// A bootstrapped `owner` running `exe` with live `sessions`,
    /// attributed only provisionally (the aggregator admits it only under
    /// a corroborating screencast signal).
    pub(crate) fn provisional(
        &mut self,
        owner: &str,
        exe: String,
        sessions: Vec<String>,
    ) -> Vec<StreamReport> {
        self.record(owner, exe, sessions, false)
    }

    fn record(
        &mut self,
        owner: &str,
        exe: String,
        sessions: Vec<String>,
        confirmed: bool,
    ) -> Vec<StreamReport> {
        if sessions.is_empty() {
            return Vec::new();
        }
        match self.owners.get_mut(owner) {
            // Already tracked with the same exe: add the sessions and
            // re-report only on a provisional→confirmed promotion (it drops
            // the corroboration requirement). An additional session under
            // an already-live source needs no fresh report.
            Some(state) if state.exe == exe => {
                state.sessions.extend(sessions);
                if confirmed && !state.confirmed {
                    state.confirmed = true;
                    return vec![portal_report(owner, &exe, true, false)];
                }
                Vec::new()
            }
            // A different exe for a known owner: clear the prior source,
            // then report the new one with only the new sessions.
            Some(state) => {
                let old_exe = std::mem::replace(&mut state.exe, exe.clone());
                state.confirmed = confirmed;
                state.sessions = sessions.into_iter().collect();
                vec![
                    portal_report(owner, &old_exe, false, false),
                    portal_report(owner, &exe, true, !confirmed),
                ]
            }
            // Brand new owner: track it and report the source.
            None => {
                self.owners.insert(
                    owner.to_string(),
                    OwnerState {
                        exe: exe.clone(),
                        confirmed,
                        sessions: sessions.into_iter().collect(),
                    },
                );
                vec![portal_report(owner, &exe, true, !confirmed)]
            }
        }
    }

    /// The session at `path` ended: drop it from its owner's set and clear
    /// the source only when the owner's last session goes. A session not
    /// tracked (or already closed) changes nothing.
    pub(crate) fn closed(&mut self, path: &str) -> Vec<StreamReport> {
        let Some(owner) = session_owner(path) else {
            return Vec::new();
        };
        let std::collections::btree_map::Entry::Occupied(mut state) = self.owners.entry(owner)
        else {
            return Vec::new();
        };
        state.get_mut().sessions.remove(path);
        if !state.get().sessions.is_empty() {
            return Vec::new();
        }
        let (owner, state) = state.remove_entry();
        vec![portal_report(&owner, &state.exe, false, false)]
    }

    /// `owner` left the bus: every session it held is gone, so its source
    /// clears.
    pub(crate) fn gone(&mut self, owner: &str) -> Vec<StreamReport> {
        self.owners
            .remove(owner)
            .map(|state| portal_report(owner, &state.exe, false, false))
            .into_iter()
            .collect()
    }

    /// Tear down every tracked session (a monitor reconnect or a vanished
    /// portal): clear each live source so nothing lingers armed.
    pub(crate) fn clear(&mut self) -> Vec<StreamReport> {
        let reports = self
            .owners
            .iter()
            .map(|(owner, state)| portal_report(owner, &state.exe, false, false))
            .collect();
        self.owners.clear();
        reports
    }
}

/// A `Portal` source report attributed to `exe`, running as bus name
/// `owner`. The owner keeps two processes of one exe apart, so the first
/// to close can't clear the other's source. `provisional` marks a
/// bootstrapped guess the aggregator admits only under a corroborating
/// screencast signal; a confirmed report always arms.
fn portal_report(owner: &str, exe: &str, active: bool, provisional: bool) -> StreamReport {
    StreamReport {
        kind: StreamSourceKind::Portal,
        detail: format!("{exe} ({owner})"),
        active,
        provisional,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = ":1.42";
    const SESSION: &str = "/org/freedesktop/portal/desktop/session/1_42/krdp_a";
    const OTHER_SESSION: &str = "/org/freedesktop/portal/desktop/session/1_42/krdp_b";

    fn call(iface: &str, member: &str, sender: &str) -> PortalMessage {
        PortalMessage {
            ty: MsgType::MethodCall,
            interface: Some(iface.into()),
            member: Some(member.into()),
            path: Some(PORTAL_PATH.into()),
            sender: Some(sender.into()),
            session_token: None,
            vanished: None,
        }
    }

    fn create(iface: &str, sender: &str, token: &str) -> PortalMessage {
        PortalMessage {
            session_token: Some(token.into()),
            ..call(iface, CREATE_SESSION, sender)
        }
    }

    fn close(path: &str, sender: &str) -> PortalMessage {
        PortalMessage {
            path: Some(path.into()),
            ..call(SESSION_IFACE, CLOSE, sender)
        }
    }

    fn signal(iface: &str, member: &str, path: &str) -> PortalMessage {
        PortalMessage {
            ty: MsgType::Signal,
            interface: Some(iface.into()),
            member: Some(member.into()),
            path: Some(path.into()),
            sender: Some(":1.10".into()),
            session_token: None,
            vanished: None,
        }
    }

    fn vanished(name: &str) -> PortalMessage {
        PortalMessage {
            vanished: Some(name.into()),
            ..signal(BUS_NAME, NAME_OWNER_CHANGED, BUS_PATH)
        }
    }

    #[test]
    fn create_session_on_capture_interfaces_arms() {
        assert_eq!(
            classify(&create(SCREENCAST_IFACE, OWNER, "krdp_a")),
            Some(PortalEvent::Create {
                sender: OWNER.into(),
                session: SESSION.into(),
            })
        );
        assert_eq!(
            classify(&create(REMOTE_DESKTOP_IFACE, ":1.234", "t1")),
            Some(PortalEvent::Create {
                sender: ":1.234".into(),
                session: "/org/freedesktop/portal/desktop/session/1_234/t1".into(),
            })
        );
    }

    #[test]
    fn a_create_session_the_portal_refuses_is_ignored() {
        // No token, a token outside [A-Za-z0-9_], or an empty one: the
        // call opens no session.
        assert_eq!(
            classify(&call(SCREENCAST_IFACE, CREATE_SESSION, OWNER)),
            None
        );
        assert_eq!(classify(&create(SCREENCAST_IFACE, OWNER, "a/b")), None);
        assert_eq!(classify(&create(SCREENCAST_IFACE, OWNER, "")), None);
    }

    #[test]
    fn input_capture_create_session_is_ignored() {
        // lan-mouse holds a standing InputCapture session while the user
        // is physically present — it shares the object path and the
        // CreateSession member, so only the interface filter excludes it.
        assert_eq!(
            classify(&create("org.freedesktop.portal.InputCapture", ":1.56", "t")),
            None
        );
    }

    #[test]
    fn non_create_portal_methods_are_ignored() {
        // Start/SelectSources/etc. are not the arming edge.
        assert_eq!(classify(&call(SCREENCAST_IFACE, "Start", OWNER)), None);
        assert_eq!(
            classify(&call(SCREENCAST_IFACE, "SelectSources", OWNER)),
            None
        );
        assert_eq!(
            classify(&call(REMOTE_DESKTOP_IFACE, "ConnectToEIS", OWNER)),
            None
        );
    }

    #[test]
    fn session_closed_signal_disarms() {
        assert_eq!(
            classify(&signal(SESSION_IFACE, CLOSED, SESSION)),
            Some(PortalEvent::Closed {
                path: SESSION.into()
            })
        );
        // A different signal on the session object is not a close.
        assert_eq!(classify(&signal(SESSION_IFACE, "Response", SESSION)), None);
    }

    /// The portal tears a session down on its owner's `Close` without
    /// emitting `Closed`, so the call itself is the disarming edge.
    #[test]
    fn the_owners_close_call_disarms() {
        assert_eq!(
            classify(&close(SESSION, OWNER)),
            Some(PortalEvent::Closed {
                path: SESSION.into()
            })
        );
        // The portal refuses anyone else's Close.
        assert_eq!(classify(&close(SESSION, ":1.43")), None);
    }

    /// A client's exit ends its sessions silently too; the bus driver's
    /// `NameOwnerChanged` is the edge.
    #[test]
    fn a_vanished_owner_or_portal_disarms() {
        assert_eq!(
            classify(&vanished(OWNER)),
            Some(PortalEvent::OwnerGone {
                owner: OWNER.into()
            })
        );
        assert_eq!(
            classify(&vanished(PORTAL_NAME)),
            Some(PortalEvent::PortalGone)
        );
        // Another well-known name going is not the portal's.
        assert_eq!(classify(&vanished("org.kde.krdpserver")), None);
        // A name changing hands (no vanished name) is ignored.
        assert_eq!(
            classify(&signal(BUS_NAME, NAME_OWNER_CHANGED, BUS_PATH)),
            None
        );
    }

    #[test]
    fn allow_list_matches_only_the_known_stream_apps() {
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
        let r = t.opened(OWNER, "krdpserver".into(), SESSION.into());
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert!(!r[0].provisional, "a confirmed open always arms");
        assert_eq!(r[0].detail, "krdpserver (:1.42)");
        // The single session closes: the source clears.
        let r = t.closed(SESSION);
        assert_eq!(r.len(), 1);
        assert!(!r[0].active);
        // The portal's Closed after the owner's Close is a no-op.
        assert!(t.closed(SESSION).is_empty());
        // Closing an unknown owner's session is a no-op.
        assert!(
            t.closed("/org/freedesktop/portal/desktop/session/1_99/t")
                .is_empty()
        );
    }

    #[test]
    fn tracker_keeps_concurrent_sessions_per_owner() {
        let mut t = PortalTracker::default();
        // krdp opens two capture sessions (ScreenCast + RemoteDesktop) from
        // one bus name: the first reports the source active, the second is
        // an already-armed no-op.
        let r = t.opened(OWNER, "krdpserver".into(), SESSION.into());
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert!(
            t.opened(OWNER, "krdpserver".into(), OTHER_SESSION.into())
                .is_empty()
        );
        // Closing the first session, even twice (Close, then Closed), must
        // NOT clear the source — the other session is still capturing.
        assert!(t.closed(SESSION).is_empty());
        assert!(
            t.closed(SESSION).is_empty(),
            "a session seen closing twice counts once"
        );
        // Closing the last session clears it.
        let r = t.closed(OTHER_SESSION);
        assert_eq!(r.len(), 1);
        assert!(!r[0].active);
        assert_eq!(r[0].detail, "krdpserver (:1.42)");
    }

    #[test]
    fn tracker_clears_an_owner_that_left_with_sessions_open() {
        let mut t = PortalTracker::default();
        t.opened(OWNER, "krdpserver".into(), SESSION.into());
        t.opened(OWNER, "krdpserver".into(), OTHER_SESSION.into());
        let r = t.gone(OWNER);
        assert_eq!(r.len(), 1);
        assert!(!r[0].active);
        assert_eq!(r[0].detail, "krdpserver (:1.42)");
        // Already gone: nothing left to clear.
        assert!(t.gone(OWNER).is_empty());
        assert!(t.closed(SESSION).is_empty());
    }

    #[test]
    fn tracker_bootstrap_is_provisional_until_confirmed() {
        let mut t = PortalTracker::default();
        // A bootstrapped owner reports provisionally.
        let r = t.provisional(OWNER, "rustdesk".into(), vec![SESSION.into()]);
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert!(
            r[0].provisional,
            "a bootstrapped source needs corroboration"
        );
        // A later CreateSession on the wire promotes it to confirmed —
        // re-reported as non-provisional so the aggregator drops the
        // corroboration requirement.
        let r = t.opened(OWNER, "rustdesk".into(), OTHER_SESSION.into());
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert!(!r[0].provisional);
        // The bootstrapped session closing leaves the confirmed one live.
        assert!(t.closed(SESSION).is_empty());
        assert_eq!(t.closed(OTHER_SESSION).len(), 1);
        // An owner with no sessions bootstraps nothing.
        assert!(
            t.provisional(":1.43", "rustdesk".into(), Vec::new())
                .is_empty()
        );
    }

    #[test]
    fn tracker_clear_tears_down_every_live_source() {
        let mut t = PortalTracker::default();
        t.opened(OWNER, "krdpserver".into(), SESSION.into());
        t.provisional(
            ":1.43",
            "rustdesk".into(),
            vec!["/org/freedesktop/portal/desktop/session/1_43/t".into()],
        );
        let mut r = t.clear();
        r.sort_by(|a, b| a.detail.cmp(&b.detail));
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|report| !report.active));
        assert_eq!(r[0].detail, "krdpserver (:1.42)");
        assert_eq!(r[1].detail, "rustdesk (:1.43)");
        // A cleared tracker has nothing left to tear down.
        assert!(t.clear().is_empty());
    }

    /// Two processes of one exe (a RustDesk reconnect under a new bus
    /// name before the old one's session closes) are two sources: the
    /// first to close leaves the other armed.
    #[test]
    fn tracker_keeps_owners_of_one_exe_apart() {
        let mut t = PortalTracker::default();
        let first = t.opened(OWNER, "rustdesk".into(), SESSION.into());
        let second = t.opened(
            ":1.43",
            "rustdesk".into(),
            "/org/freedesktop/portal/desktop/session/1_43/t".into(),
        );
        assert_ne!(first[0].detail, second[0].detail);
        let r = t.closed(SESSION);
        assert_eq!(r.len(), 1);
        assert_eq!(
            r[0].detail, first[0].detail,
            "only the closed owner's source clears"
        );
    }
}
