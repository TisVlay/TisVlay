// The CLI prints its results to stdout by design; the workspace print
// lints target the daemons and libraries, which log through tracing.
// `allow`, not `expect`: the bin's test build doesn't trip `print_stderr`.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the CLI prints its output"
)]

mod pairing;
mod profiles;
mod reload;
mod remote;
mod render;
mod status;
mod tvs;

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use tisvlay_cli::{Cli, Command};
use tisvlay_protocol::caller::{Caller, RpcError};
use tisvlay_protocol::endpoint::{Unary, manager, tv, user_profile, vendors};
use tisvlay_protocol::rpc;
use zenoh::Session;

use crate::pairing::{
    address_config, discover, discover_session, pair, pairing_config, watch_pairing,
};
use crate::profiles::{profile_cmd, user_profile_cmd};
use crate::reload::reload;
use crate::remote::remote_session_cmd;
use crate::render::print_reply;
use crate::status::status;
use crate::tvs::{list, watch};

fn main() -> Result<()> {
    // Answer completion requests before building a runtime. The dynamic
    // engine shells back into us per <TAB>, and the value completers query
    // the daemon on their own runtime — which would panic under an ambient
    // one. A normal run (COMPLETE unset) falls straight through.
    CompleteEnv::with_factory(Cli::command)
        .completer("tisvlay")
        .complete();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

/// `enable-access`: enroll the current user in the daemon's socket group via
/// a polkit-authenticated `usermod`, printing the outcome.
fn enable_access(socket: &Path) -> Result<()> {
    use tisvlay_runtime::access::{self, EnrollOutcome};
    // Gate on the durable grant (group membership), never on reachability:
    // a console user's presence grant makes the socket connectable, yet
    // they are exactly who runs this verb to gain SSH/remote access.
    if access::has_durable_access(socket) {
        println!("You already have durable TV control access (group member).");
        return Ok(());
    }
    println!("Granting TV control access — authenticate as an administrator when prompted…");
    match access::enable_access(socket)? {
        EnrollOutcome::Enrolled { user, group } => {
            println!("Added '{user}' to the '{group}' group.");
            println!(
                "Log out and back in for it to take effect — a reboot is the most reliable, \
                 since the systemd user manager can outlive a logout and keep the old groups."
            );
        }
        // The durable gate read false a moment ago; only a race lands here.
        EnrollOutcome::AlreadyMember { .. } => {
            println!("You already have durable TV control access.");
        }
    }
    Ok(())
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let socket = cli
        .socket
        .unwrap_or_else(tisvlay_protocol::bootstrap::default_connect_socket_path);
    // `enable-access` needs no daemon connection — it is the fix for not
    // being able to reach the socket, so it runs before opening the client.
    if matches!(cli.command, Command::EnableAccess) {
        return enable_access(&socket);
    }
    let session = open_client(&socket).await?;
    let site = &cli.site;
    let caller = Caller::new(session.clone(), site.as_str());
    let json_out = cli.json;
    // Commands that send the daemon rpcs check its release first; `status`
    // reports it, `watch` is a passive view, the user-socket commands
    // check the session service instead, and `reload` checks each service
    // it reloads.
    if !matches!(
        cli.command,
        Command::Status
            | Command::Watch
            | Command::UserProfile(_)
            | Command::RemoteSession(_)
            | Command::Reload
    ) {
        check_release::<manager::Info>(&caller, "daemon").await?;
    }
    match cli.command {
        // Handled above, before the client opens (needs no connection).
        Command::EnableAccess => unreachable!(),
        Command::Status => status(&session, &caller, site, &socket, json_out).await,
        Command::Reload => reload(&caller, cli.user_socket, site, json_out).await,
        Command::List => list(&session, &caller, site, json_out).await,
        Command::Watch => watch(&session, site).await,
        Command::Discover { vendor, timeout_ms } => {
            discover(&caller, vendor, timeout_ms, json_out).await
        }
        Command::Pair {
            name,
            vendor,
            params,
            address,
        } => {
            let config = pairing_config(params, address)?;
            pair(&session, &caller, site, name, vendor, config).await
        }
        Command::Add {
            name,
            address,
            vendor,
            unmanaged,
        } => {
            let request = rpc::AddTv {
                id: None,
                name,
                vendor,
                managed: !unmanaged,
                config: address_config(address),
            };
            let created = call::<manager::AddTv>(&caller, &(), &request).await?;
            println!("added: {}", created.id);
            Ok(())
        }
        Command::SetVolume { id, level } => {
            ack::<tv::SetVolume>(&caller, &id, &rpc::SetVolume { level }).await
        }
        Command::Input { id, input } => {
            ack::<tv::SwitchInput>(&caller, &id, &rpc::SwitchInput { input }).await
        }
        Command::Backlight { id, level } => {
            ack::<tv::SetBacklight>(&caller, &id, &rpc::SetBacklight { level }).await
        }
        Command::Screen { id, state } => {
            let request = rpc::SetScreen::manual(state == "on");
            ack::<tv::SetScreen>(&caller, &id, &request).await
        }
        Command::Rename { id, name } => {
            ack::<tv::SetName>(&caller, &id, &rpc::SetName { name }).await
        }
        Command::Managed { id, managed } => {
            ack::<tv::SetManaged>(&caller, &id, &rpc::SetManaged { managed }).await
        }
        Command::Config { id } => {
            let config = call::<tv::GetConfig>(&caller, &id, &()).await?;
            print_reply(&config, json_out)
        }
        Command::ConfigSet { id, patch } => {
            let patch: rpc::UpdateConfig = serde_json::from_str(&patch)
                .map_err(|e| anyhow!("the patch must be a JSON object: {e}"))?;
            ack::<tv::UpdateConfig>(&caller, &id, &patch).await
        }
        Command::Repair { id } => {
            let started = call::<tv::RePair>(&caller, &id, &()).await?;
            println!("re-pairing session {id} started");
            watch_pairing(&session, &caller, site, id, started.attempt).await
        }
        Command::Power { id, state } => {
            if state == "on" {
                ack::<tv::PowerOn>(&caller, &id, &rpc::PowerOn::default()).await
            } else {
                ack::<tv::PowerOff>(&caller, &id, &()).await
            }
        }
        Command::Reconnect { id } => ack::<tv::Reconnect>(&caller, &id, &()).await,
        Command::Remove { id } => {
            call::<tv::Remove>(&caller, &id, &()).await?;
            println!("removed");
            Ok(())
        }
        Command::Mute { id, muted } => {
            ack::<tv::SetMuted>(&caller, &id, &rpc::SetMuted { muted }).await
        }
        Command::PrimaryInput { id, input } => {
            ack::<tv::SetPrimaryInput>(&caller, &id, &rpc::SetPrimaryInput { input }).await
        }
        Command::Assign { id, profile } => {
            ack::<tv::SetSystemProfile>(&caller, &id, &rpc::SetSystemProfile { profile }).await
        }
        Command::Vendors => {
            let vendors = call::<vendors::Catalog>(&caller, &(), &()).await?;
            if json_out {
                return print_reply(&vendors, true);
            }
            for vendor in vendors {
                println!(
                    "{}  {}  [{}]",
                    vendor.id,
                    vendor.name,
                    vendor.capabilities.join(", "),
                );
            }
            Ok(())
        }
        Command::Schema { vendor } => {
            let schema = call::<vendors::Schema>(&caller, vendor.as_str(), &()).await?;
            if json_out {
                return print_reply(&schema, true);
            }
            for field in schema {
                let required = if field.required { " (required)" } else { "" };
                println!("{}{}  {}", field.name, required, field.helper,);
            }
            Ok(())
        }
        Command::DiscoverSession { vendor } => {
            discover_session(&session, &caller, site, vendor, json_out).await
        }
        Command::Profile(cmd) => profile_cmd(&session, &caller, site, cmd, json_out).await,
        Command::UserProfile(cmd) => {
            let user_socket = cli
                .user_socket
                .unwrap_or_else(tisvlay_protocol::bootstrap::default_session_socket_path);
            let user = zenoh::open(tisvlay_protocol::bootstrap::client_config(&user_socket)?)
                .await
                .map_err(|e| {
                    anyhow!(
                        "session service unreachable at {}: {e}",
                        user_socket.display()
                    )
                })?;
            let user_caller = Caller::new(user.clone(), site.as_str());
            check_release::<user_profile::Info>(&user_caller, "session service").await?;
            user_profile_cmd(&user, &user_caller, site, cmd, json_out).await
        }
        Command::RemoteSession(cmd) => {
            let user_socket = cli
                .user_socket
                .unwrap_or_else(tisvlay_protocol::bootstrap::default_session_socket_path);
            let user = zenoh::open(tisvlay_protocol::bootstrap::client_config(&user_socket)?)
                .await
                .map_err(|e| {
                    anyhow!(
                        "session service unreachable at {}: {e}",
                        user_socket.display()
                    )
                })?;
            let user_caller = Caller::new(user.clone(), site.as_str());
            check_release::<user_profile::Info>(&user_caller, "session service").await?;
            remote_session_cmd(&user_caller, cmd, json_out).await
        }
    }
}

async fn open_client(socket: &Path) -> Result<Session> {
    zenoh::open(tisvlay_protocol::bootstrap::client_config(socket)?)
        .await
        .map_err(|e| {
            // A connect failure for a caller who isn't in the socket group
            // reads as "daemon unreachable", but the real cause is access:
            // the group-owned runtime dir isn't even searchable, so socket
            // resolution can't see the live daemon and lands on the dev
            // path. Point at the fix instead of the misleading path.
            if !tisvlay_runtime::access::has_access(socket) {
                anyhow!(
                    "You don't have TV control access yet — run `tisvlay enable-access` to grant it."
                )
            } else {
                anyhow!("daemon unreachable at {}: {e}", socket.display())
            }
        })
}

/// One typed rpc round-trip; a failure keeps its cause chain for the
/// terminal.
async fn call<R: Unary>(
    caller: &Caller,
    target: &R::Target,
    request: &R::Request,
) -> Result<R::Response> {
    Ok(caller.call::<R>(target, request).await?)
}

/// One acknowledged rpc: call, print the human ack.
async fn ack<R: Unary<Response = rpc::Ack>>(
    caller: &Caller,
    target: &R::Target,
    request: &R::Request,
) -> Result<()> {
    call::<R>(caller, target, request).await?;
    println!("ok");
    Ok(())
}

/// Refuse to send rpcs to a service on a release this CLI can't talk to
/// (a different `major.minor`), or to one that isn't there at all, which
/// fails here rather than waiting out the command's own rpc too. A service
/// that's there but doesn't answer `info` isn't checked: the command
/// reports that itself.
async fn check_release<R>(caller: &Caller, service: &str) -> Result<()>
where
    R: Unary<Target = (), Request = (), Response = rpc::ServiceInfo>,
{
    match release_mismatch::<R>(caller, service).await {
        Err(e) => bail!("the {service} isn't running ({e})"),
        Ok(Some(mismatch)) => bail!("{mismatch}"),
        Ok(None) => Ok(()),
    }
}

/// The mismatch message when `service` answers its `info` rpc with a
/// release this CLI can't talk to. `Err` when no service is there at all
/// ([`RpcError::Unavailable`]); any other failure to ask counts as no
/// mismatch, for the command to report.
async fn release_mismatch<R>(
    caller: &Caller,
    service: &str,
) -> std::result::Result<Option<String>, RpcError>
where
    R: Unary<Target = (), Request = (), Response = rpc::ServiceInfo>,
{
    let info = match caller.call::<R>(&(), &()).await {
        Ok(info) => info,
        Err(e @ RpcError::Unavailable { .. }) => return Err(e),
        Err(_) => return Ok(None),
    };
    Ok((!tisvlay_protocol::compatible(tisvlay_protocol::VERSION, &info.version)).then(|| {
        format!(
            "the {service} runs release {}, which this CLI ({}) can't talk to; restart it or upgrade so they match",
            info.version,
            tisvlay_protocol::VERSION
        )
    }))
}
