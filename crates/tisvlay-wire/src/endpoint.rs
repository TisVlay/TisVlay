//! Typed rpc endpoints: every rpc is declared once, here, as a key plus
//! its request and response types, and both sides derive from that
//! declaration. Clients call through [`Caller`](crate::caller::Caller)
//! with the marker type. Servers receive a query as their family's
//! `Incoming` enum and match it exhaustively: each variant carries the
//! decoded request and a [`Responder`] typed by that rpc, so a family
//! member without a handler, a handler fed the wrong body type, or a
//! reply of another rpc's type doesn't compile.

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::error::WireError;
use crate::serve::Responder;
use crate::{keys, rpc, serve};

/// One rpc: where it lives and what it carries.
pub trait Rpc {
    /// What addresses a call besides the site: `()`, a TV id, a vendor id.
    type Target: ?Sized;
    type Request: Serialize + DeserializeOwned;
    type Response: Serialize + DeserializeOwned;
    /// The rpc's name within its family, for logs.
    const NAME: &'static str;
    /// The rpc's key on `site`.
    fn key(site: &str, target: &Self::Target) -> String;
}

/// An rpc answered by exactly one reply.
pub trait Unary: Rpc {
    /// Safe to re-issue when a matched query closes without a reply:
    /// reads, which neither mint state nor apply a relative effect.
    const REISSUE: bool;
}

/// An rpc answered by one reply per finding, collected until the window
/// closes.
pub trait Streaming: Rpc {
    /// How long to collect replies for this request.
    fn window(request: &Self::Request) -> Duration;
}

/// A family key parser's result: the verb, plus the target for a family
/// whose keys carry one (a TV id).
pub(crate) trait ParsedKey<'a> {
    type Target;
    fn split(self) -> (Self::Target, &'a str);
}

impl<'a> ParsedKey<'a> for &'a str {
    type Target = ();
    fn split(self) -> ((), &'a str) {
        ((), self)
    }
}

impl<'a> ParsedKey<'a> for (Uuid, &'a str) {
    type Target = Uuid;
    fn split(self) -> (Uuid, &'a str) {
        self
    }
}

/// Decode an rpc body; an absent payload reads as JSON `null`.
fn decode_body<T: DeserializeOwned>(payload: &[u8]) -> Result<T, WireError> {
    let payload = if payload.is_empty() {
        b"null".as_slice()
    } else {
        payload
    };
    serde_json::from_slice(payload).map_err(|e| WireError::bad_request(e.to_string()))
}

/// Declare one family of verb-shaped rpcs: a marker type per rpc
/// implementing [`Rpc`] plus [`Unary`] (`read`/`write`) or nothing
/// (`stream`, whose [`Streaming`] impl is written by hand); the family's
/// `Call` enum carrying each rpc's decoded request; and its `Incoming`
/// enum pairing that request with the rpc's [`Responder`].
///
/// `key` builds an rpc's key and `parse` splits one back into its target
/// and verb; `wild` is the wildcard the family's queryable declares. The
/// `key` form reads like a closure but is spliced into each marker's
/// [`Rpc::key`], with `verb` bound to that rpc's verb string.
macro_rules! family {
    (
        $(#[$family_meta:meta])*
        target: $target:ty,
        key: |$site:ident, $tgt:ident, $verb:ident| $key:expr,
        parse: $parse:path,
        wild: $wild:path,
        rpcs: {
            $(
                $(#[$meta:meta])*
                $name:ident = $verb_str:literal ($request:ty) -> $response:ty, $kind:ident;
            )*
        }
    ) => {
        $(
            $(#[$meta])*
            #[derive(Debug, Clone, Copy)]
            pub struct $name;

            impl $crate::endpoint::Rpc for $name {
                type Target = $target;
                type Request = $request;
                type Response = $response;
                const NAME: &'static str = $verb_str;
                fn key($site: &str, $tgt: &$target) -> String {
                    let $verb = $verb_str;
                    $key
                }
            }

            family!(@kind $name $kind);
        )*

        $(#[$family_meta])*
        #[derive(Debug)]
        pub enum Call {
            $( $name($request), )*
        }

        impl Call {
            /// Decode a query's verb and body into the rpc it names.
            pub fn decode(verb: &str, payload: &[u8]) -> Result<Self, $crate::error::WireError> {
                match verb {
                    $( $verb_str => $crate::endpoint::decode_body(payload).map(Self::$name), )*
                    other => Err($crate::error::WireError::unknown_rpc(other)),
                }
            }
        }

        /// A received rpc of this family: its decoded request and the reply
        /// it owes.
        #[derive(Debug)]
        pub enum Incoming {
            $( $name($request, $crate::serve::Responder<$name>), )*
        }

        impl Incoming {
            /// The wildcard this family's queryable declares.
            pub fn wild(site: &str) -> String {
                $wild(site)
            }

            /// Receive a query on this family. A key naming no rpc of the
            /// family, or a body that doesn't decode, is refused here and
            /// yields `None`.
            pub async fn receive(site: &str, query: zenoh::query::Query) -> Option<($target, Self)> {
                let parsed = $parse(site, query.key_expr().as_str())
                    .map($crate::endpoint::ParsedKey::split)
                    .map(|(target, verb)| (target, verb.to_string()));
                let Some((target, verb)) = parsed else {
                    let error = $crate::error::WireError::unknown_rpc(query.key_expr().as_str());
                    $crate::serve::reply_err(&query, &error).await;
                    return None;
                };
                match Call::decode(&verb, &$crate::serve::payload(&query)) {
                    Ok(call) => Some((target, Self::with_responder(call, query))),
                    Err(error) => {
                        $crate::serve::reply_err(&query, &error).await;
                        None
                    }
                }
            }

            fn with_responder(call: Call, query: zenoh::query::Query) -> Self {
                match call {
                    $( Call::$name(request) => {
                        Self::$name(request, $crate::serve::Responder::new(query))
                    } )*
                }
            }
        }
    };
    (@kind $name:ident read) => {
        impl $crate::endpoint::Unary for $name {
            const REISSUE: bool = true;
        }
    };
    (@kind $name:ident write) => {
        impl $crate::endpoint::Unary for $name {
            const REISSUE: bool = false;
        }
    };
    (@kind $name:ident stream) => {};
}

/// The daemon's manager rpcs: `manager/rpc/<verb>`.
pub mod manager {
    use super::*;

    family! {
        /// A decoded manager rpc.
        target: (),
        key: |site, _target, verb| keys::manager_rpc(site, verb),
        parse: keys::parse_manager_rpc,
        wild: keys::manager_rpc_wild,
        rpcs: {
            /// The daemon's version and state path.
            Info = "info" (()) -> rpc::ServiceInfo, read;
            /// Apply a hand edit of the daemon's store file.
            Reload = "reload" (()) -> rpc::ReloadReport, write;
            /// A MAC hint for a manually entered TV, from the ARP cache.
            Enrich = "enrich" (rpc::Enrich) -> rpc::EnrichReply, read;
            /// A bounded scan; each finding streams back as its own reply.
            Discover = "discover" (rpc::Discover) -> tisvlay_core::DiscoveredTv, stream;
            /// Open a scan session publishing findings until stopped.
            DiscoverStart = "discover-start" (rpc::DiscoverStart) -> rpc::DiscoverySession, write;
            DiscoverStop = "discover-stop" (rpc::DiscoverStop) -> rpc::Ack, write;
            /// Enroll a TV from a full config, without pairing.
            AddTv = "add-tv" (rpc::AddTv) -> rpc::Created, write;
            /// Pair a new TV; progress streams on its pairing key.
            Pair = "pair" (rpc::AddTv) -> rpc::PairStarted, write;
            CancelPair = "cancel-pair" (rpc::TvRef) -> rpc::Ack, write;
        }
    }

    /// Margin over the scan window: the daemon replies as findings land,
    /// and the query stays open until its last vendor scan finishes.
    pub(super) const DISCOVER_MARGIN: Duration = Duration::from_secs(4);

    /// The scan window the daemon uses when the request names none.
    const DISCOVER_DEFAULT_MS: u64 = 3000;

    /// The longest scan window the daemon honors, under the zenoh query
    /// timeout ceiling.
    pub(super) const DISCOVER_MAX_MS: u64 = 8000;

    /// How long the daemon scans for a bounded discover: the requested
    /// window (or the default), clamped under the query timeout.
    pub fn discover_scan(request: &rpc::Discover) -> Duration {
        let scan = request
            .timeout_ms
            .unwrap_or(DISCOVER_DEFAULT_MS)
            .min(DISCOVER_MAX_MS);
        Duration::from_millis(scan)
    }

    impl Streaming for Discover {
        fn window(request: &rpc::Discover) -> Duration {
            discover_scan(request) + DISCOVER_MARGIN
        }
    }
}

/// The daemon's system-profile rpcs: `profile/rpc/<verb>`.
pub mod profile {
    use super::*;

    family! {
        /// A decoded system-profile rpc.
        target: (),
        key: |site, _target, verb| keys::profile_rpc(site, verb),
        parse: keys::parse_profile_rpc,
        wild: keys::profile_rpc_wild,
        rpcs: {
            Create = "create" (rpc::ProfileCreate) -> rpc::Created, write;
            Update = "update" (rpc::ProfileUpdate) -> rpc::Ack, write;
            Duplicate = "duplicate" (rpc::ProfileDuplicate) -> rpc::Created, write;
            Delete = "delete" (rpc::TvRef) -> rpc::Ack, write;
            /// The stock event mapping an unset profile field inherits.
            Defaults = "defaults" (()) -> tisvlay_core::SystemProfileConfig, read;
            /// One TV's resolved mapping.
            Effective = "effective" (rpc::TvRef) -> tisvlay_core::SystemProfileConfig, read;
        }
    }
}

/// The daemon's per-TV rpcs: `tv/<uuid>/rpc/<verb>`.
pub mod tv {
    use super::*;

    family! {
        /// A decoded per-TV rpc.
        target: Uuid,
        key: |site, tv, verb| keys::tv_rpc(site, tv, verb),
        parse: keys::parse_tv_rpc,
        wild: keys::tv_rpc_wild,
        rpcs: {
            SetVolume = "set-volume" (rpc::SetVolume) -> rpc::Ack, write;
            SetMuted = "set-muted" (rpc::SetMuted) -> rpc::Ack, write;
            SetName = "set-name" (rpc::SetName) -> rpc::Ack, write;
            SetManaged = "set-managed" (rpc::SetManaged) -> rpc::Ack, write;
            SetSystemProfile = "set-system-profile" (rpc::SetSystemProfile) -> rpc::Ack, write;
            SetPrimaryInput = "set-primary-input" (rpc::SetPrimaryInput) -> rpc::Ack, write;
            SwitchInput = "switch-input" (rpc::SwitchInput) -> rpc::Ack, write;
            SetBacklight = "set-backlight" (rpc::SetBacklight) -> rpc::Ack, write;
            SetScreen = "set-screen" (rpc::SetScreen) -> rpc::Ack, write;
            PowerOn = "power-on" (rpc::PowerOn) -> rpc::Ack, write;
            PowerOff = "power-off" (()) -> rpc::Ack, write;
            Reconnect = "reconnect" (()) -> rpc::Ack, write;
            Remove = "remove" (()) -> rpc::Ack, write;
            /// The TV's config, secrets removed.
            GetConfig = "get-config" (()) -> rpc::TvConfigView, read;
            UpdateConfig = "update-config" (rpc::UpdateConfig) -> rpc::Ack, write;
            /// Re-run pairing to refresh credentials.
            RePair = "re-pair" (()) -> rpc::RePairStarted, write;
        }
    }
}

/// The session's user-profile rpcs (user socket): `user/profile/rpc/<verb>`.
pub mod user_profile {
    use super::*;

    family! {
        /// A decoded user-profile rpc.
        target: (),
        key: |site, _target, verb| keys::user_profile_rpc(site, verb),
        parse: keys::parse_user_profile_rpc,
        wild: keys::user_profile_rpc_wild,
        rpcs: {
            Create = "create" (rpc::UserProfileCreate) -> rpc::Created, write;
            Update = "update" (rpc::UserProfileUpdate) -> rpc::Ack, write;
            Duplicate = "duplicate" (rpc::ProfileDuplicate) -> rpc::Created, write;
            Delete = "delete" (rpc::TvRef) -> rpc::Ack, write;
            /// The session's version and state path.
            Info = "info" (()) -> rpc::ServiceInfo, read;
            /// The hard-coded floor an unset profile field inherits.
            Defaults = "defaults" (()) -> tisvlay_core::UserProfileConfig, read;
            /// One TV's resolved user mapping.
            Effective = "effective" (rpc::TvRef) -> tisvlay_core::ResolvedUserProfile, read;
            /// Assign (or unassign) a user profile to a TV.
            Assign = "assign" (rpc::UserAssign) -> rpc::Ack, write;
            /// Apply a hand edit of the session's store file.
            Reload = "reload" (()) -> rpc::ReloadReport, write;
        }
    }
}

/// The session's remote-session rpcs (user socket): `user/remote/rpc/<verb>`.
pub mod user_remote {
    use super::*;

    family! {
        /// A decoded remote-session rpc.
        target: (),
        key: |site, _target, verb| keys::user_remote_rpc(site, verb),
        parse: keys::parse_user_remote_rpc,
        wild: keys::user_remote_rpc_wild,
        rpcs: {
            /// Toggle a manual remote-session source; replies the new state.
            Set = "set" (rpc::RemoteSet) -> crate::payload::RemoteSessionState, write;
            Status = "status" (()) -> crate::payload::RemoteSessionState, read;
        }
    }
}

/// The tray's settings rpcs (user socket): `user/tray/rpc/<verb>`.
pub mod user_tray {
    use super::*;

    family! {
        /// A decoded tray-settings rpc.
        target: (),
        key: |site, _target, verb| keys::user_tray_rpc(site, verb),
        parse: keys::parse_user_tray_rpc,
        wild: keys::user_tray_rpc_wild,
        rpcs: {
            Get = "get" (()) -> rpc::NotificationSettings, read;
            /// Apply a partial update; replies the merged settings.
            Update = "update" (rpc::NotificationSettingsPatch) -> rpc::NotificationSettings, write;
            /// Show a test notification.
            Test = "test" (()) -> rpc::Ack, write;
            /// The tray's version and state path.
            Info = "info" (()) -> rpc::ServiceInfo, read;
            /// Apply a hand edit of the tray's settings file.
            Reload = "reload" (()) -> rpc::ReloadReport, write;
        }
    }
}

/// The daemon's vendor catalog: `manager/vendors` and
/// `manager/vendors/<vendor>/schema`. Not verb-shaped (the vendor sits
/// mid-key), so declared by hand.
pub mod vendors {
    use super::*;

    /// The vendor catalog: every registered backend.
    #[derive(Debug, Clone, Copy)]
    pub struct Catalog;

    impl Rpc for Catalog {
        type Target = ();
        type Request = ();
        type Response = Vec<rpc::VendorEntry>;
        const NAME: &'static str = "catalog";
        fn key(site: &str, _target: &()) -> String {
            keys::vendors(site)
        }
    }

    impl Unary for Catalog {
        const REISSUE: bool = true;
    }

    /// One vendor's pairing form.
    #[derive(Debug, Clone, Copy)]
    pub struct Schema;

    impl Rpc for Schema {
        type Target = str;
        type Request = ();
        type Response = Vec<tisvlay_core::PairingField>;
        const NAME: &'static str = "schema";
        fn key(site: &str, vendor: &str) -> String {
            keys::vendor_schema(site, vendor)
        }
    }

    impl Unary for Schema {
        const REISSUE: bool = true;
    }

    /// A decoded vendor-catalog rpc.
    #[derive(Debug)]
    pub enum Call {
        Catalog(()),
        Schema { vendor: String },
    }

    impl Call {
        /// Decode a query on the vendors family from its full key.
        pub fn decode(site: &str, key: &str, payload: &[u8]) -> Result<Self, WireError> {
            if key == keys::vendors(site) {
                return decode_body(payload).map(Self::Catalog);
            }
            let schema = key
                .strip_prefix(&format!("{}/", keys::vendors(site)))
                .and_then(|rest| rest.strip_suffix("/schema"));
            match schema {
                Some(vendor) => {
                    decode_body::<()>(payload)?;
                    Ok(Self::Schema {
                        vendor: vendor.to_string(),
                    })
                }
                None => Err(WireError::unknown_rpc(key)),
            }
        }
    }

    /// A received vendor-catalog rpc and the reply it owes.
    #[derive(Debug)]
    pub enum Incoming {
        Catalog(Responder<Catalog>),
        Schema {
            vendor: String,
            responder: Responder<Schema>,
        },
    }

    impl Incoming {
        /// The wildcard the catalog queryable declares; `**` spans zero
        /// chunks too, so it matches the bare catalog key.
        pub fn wild(site: &str) -> String {
            keys::vendors_wild(site)
        }

        /// Receive a query on the catalog. One that names no catalog rpc,
        /// or whose body doesn't decode, is refused here and yields `None`.
        pub async fn receive(site: &str, query: zenoh::query::Query) -> Option<Self> {
            let decoded = Call::decode(site, query.key_expr().as_str(), &serve::payload(&query));
            match decoded {
                Ok(Call::Catalog(())) => Some(Self::Catalog(Responder::new(query))),
                Ok(Call::Schema { vendor }) => Some(Self::Schema {
                    vendor,
                    responder: Responder::new(query),
                }),
                Err(error) => {
                    serve::reply_err(&query, &error).await;
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn keys_follow_the_family_shapes() {
        let tv = Uuid::nil();
        assert_eq!(
            tv::SetVolume::key("main", &tv),
            format!("tisvlay/main/tv/{tv}/rpc/set-volume")
        );
        assert_eq!(
            manager::Pair::key("main", &()),
            "tisvlay/main/manager/rpc/pair"
        );
        assert_eq!(
            vendors::Schema::key("main", "lg"),
            "tisvlay/main/manager/vendors/lg/schema"
        );
        assert_eq!(
            user_tray::Update::key("main", &()),
            "tisvlay/main/user/tray/rpc/update"
        );
    }

    #[test]
    fn calls_decode_by_verb() {
        let call = tv::Call::decode("set-volume", br#"{"level":30}"#).unwrap();
        assert!(matches!(
            call,
            tv::Call::SetVolume(rpc::SetVolume { level: 30 })
        ));
        assert!(matches!(
            tv::Call::decode("power-off", b"").unwrap(),
            tv::Call::PowerOff(())
        ));
    }

    #[test]
    fn unknown_verbs_and_bad_bodies_are_refused() {
        let unknown = tv::Call::decode("teleport", b"null").unwrap_err();
        assert_eq!(unknown.code, ErrorCode::UnknownRpc);
        let bad = tv::Call::decode("set-volume", br#"{"level":"loud"}"#).unwrap_err();
        assert_eq!(bad.code, ErrorCode::BadRequest);
    }

    #[test]
    fn vendor_keys_decode() {
        let site = "main";
        assert!(matches!(
            vendors::Call::decode(site, &keys::vendors(site), b"").unwrap(),
            vendors::Call::Catalog(())
        ));
        match vendors::Call::decode(site, &keys::vendor_schema(site, "lg"), b"").unwrap() {
            vendors::Call::Schema { vendor } => assert_eq!(vendor, "lg"),
            other => panic!("expected the schema rpc, got {other:?}"),
        }
    }

    #[test]
    fn discover_window_covers_the_scan() {
        let window = manager::Discover::window(&rpc::Discover {
            vendor: None,
            timeout_ms: Some(60_000),
        });
        assert_eq!(
            window,
            Duration::from_millis(manager::DISCOVER_MAX_MS) + manager::DISCOVER_MARGIN
        );
    }

    #[test]
    fn ack_is_an_empty_object() {
        assert_eq!(serde_json::to_string(&rpc::Ack {}).unwrap(), "{}");
    }
}

/// The typed layer's compile-time gates. Each `compile_fail` example has a
/// passing twin that differs only in what the gate checks, so a doctest
/// can't pass by failing for an unrelated reason.
///
/// A call takes its rpc's request type:
///
/// ```no_run
/// # use tisvlay_wire::{caller::Caller, endpoint::tv, rpc};
/// async fn volume(caller: &Caller, id: uuid::Uuid) {
///     let _ = caller.call::<tv::SetVolume>(&id, &rpc::SetVolume { level: 20 }).await;
/// }
/// ```
///
/// ```compile_fail,E0308
/// # use tisvlay_wire::{caller::Caller, endpoint::tv, rpc};
/// async fn volume(caller: &Caller, id: uuid::Uuid) {
///     let _ = caller.call::<tv::SetVolume>(&id, &rpc::SetMuted { muted: true }).await;
/// }
/// ```
///
/// A streaming rpc is collected, never called for one reply:
///
/// ```no_run
/// # use tisvlay_wire::{caller::Caller, endpoint::manager, rpc};
/// async fn scan(caller: &Caller) {
///     let request = rpc::Discover { vendor: None, timeout_ms: None };
///     let _ = caller.call_many::<manager::Discover>(&(), &request).await;
/// }
/// ```
///
/// ```compile_fail,E0277
/// # use tisvlay_wire::{caller::Caller, endpoint::manager, rpc};
/// async fn scan(caller: &Caller) {
///     let request = rpc::Discover { vendor: None, timeout_ms: None };
///     let _ = caller.call::<manager::Discover>(&(), &request).await;
/// }
/// ```
///
/// A server answers with its rpc's response type, and only once:
///
/// ```no_run
/// # use tisvlay_wire::{endpoint::manager, rpc, serve::Responder};
/// async fn created(responder: Responder<manager::AddTv>) {
///     responder.ok(&rpc::Created { id: uuid::Uuid::nil() }).await;
/// }
/// ```
///
/// ```compile_fail,E0308
/// # use tisvlay_wire::{endpoint::manager, rpc, serve::Responder};
/// async fn created(responder: Responder<manager::AddTv>) {
///     responder.ok(&rpc::Ack {}).await;
/// }
/// ```
///
/// ```compile_fail,E0382
/// # use tisvlay_wire::{endpoint::manager, rpc, serve::Responder};
/// async fn created(responder: Responder<manager::AddTv>) {
///     responder.ok(&rpc::Created { id: uuid::Uuid::nil() }).await;
///     responder.ok(&rpc::Created { id: uuid::Uuid::nil() }).await;
/// }
/// ```
///
/// A server handles every rpc of its family:
///
/// ```no_run
/// # use tisvlay_wire::{endpoint::user_remote, payload::RemoteSessionState};
/// async fn serve(incoming: user_remote::Incoming, state: RemoteSessionState) {
///     match incoming {
///         user_remote::Incoming::Set(_, responder) => responder.ok(&state).await,
///         user_remote::Incoming::Status((), responder) => responder.ok(&state).await,
///     }
/// }
/// ```
///
/// ```compile_fail,E0004
/// # use tisvlay_wire::{endpoint::user_remote, payload::RemoteSessionState};
/// async fn serve(incoming: user_remote::Incoming, state: RemoteSessionState) {
///     match incoming {
///         user_remote::Incoming::Status((), responder) => responder.ok(&state).await,
///     }
/// }
/// ```
#[cfg(doctest)]
mod gates {}
