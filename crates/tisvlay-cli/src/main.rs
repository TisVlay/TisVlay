// The CLI writes with `writeln!`, never `println!`/`eprintln!`, which panic
// on a closed pipe. Results are `writeln!(io::stdout(), …)?`, and `main`
// ends a run whose reader has gone quietly; diagnostics are
// `let _ = writeln!(io::stderr(), …)`, since a lost stderr has no reader to
// tell and shouldn't stop the command.
#![expect(
    unsafe_code,
    reason = "`service session` must start before any thread exists"
)]

mod attributes;
mod pairing;
mod profiles;
mod reload;
mod render;
mod status;
mod stream;
mod tvs;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use tisvlay_cli::{Cli, Command, ProfileScope, Service};
use tisvlay_protocol::caller::{Caller, RpcError};
use tisvlay_protocol::endpoint::{Unary, manager, tv, user_profile};
use tisvlay_protocol::rpc;
use zenoh::Session;

use crate::attributes::Assignment;
use crate::pairing::{discover, discover_session, pair, vendor, watch_pairing};
use crate::profiles::{profile_cmd, user_profile_cmd};
use crate::reload::reload;
use crate::status::status;
use crate::stream::stream_cmd;
use crate::tvs::{list, watch};

fn main() -> Result<()> {
    // Answer completion requests before building a runtime. The dynamic
    // engine shells back into us per <TAB>, and the value completers query
    // the daemon on their own runtime — which would panic under an ambient
    // one. A normal run (COMPLETE unset) falls straight through.
    CompleteEnv::with_factory(Cli::command)
        .completer("tisvlay")
        .complete();
    let cli = Cli::parse();
    if let Command::Service(service) = cli.command {
        return run_service(service);
    }
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(cli));
    // A reader that stopped early (`tisvlay watch | head`) isn't a failure:
    // end quietly with status 0, so a `pipefail` pipeline holds. Why not
    // SIGPIPE: docs/concerns/cli-closed-pipe.md.
    match result {
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe) =>
        {
            Ok(())
        }
        result => result,
    }
}

/// `service <name>`: the process becomes that service, which builds its
/// own runtime. It runs before the CLI builds one, so no other thread
/// exists yet.
fn run_service(service: Service) -> Result<()> {
    match service {
        Service::Daemon(args) => tisvlay_daemon::service::main(args),
        // SAFETY: called from `main` before any runtime is built, and
        // neither completion nor argument parsing spawns a thread, so this
        // is the process's only thread.
        Service::Session(args) => unsafe { tisvlay_session::service::main(args) },
        Service::Tray(args) => tisvlay_tray::service::main(args),
        Service::Greeter(args) => tisvlay_greeter::service::main(args),
        Service::Wakeup(args) => tisvlay_daemon::wakeup::main(args),
    }
}

/// `enable-access`: enroll the current user in the daemon's socket group via
/// a polkit-authenticated `usermod`, printing the outcome.
fn enable_access(socket: &Path) -> Result<()> {
    use tisvlay_runtime::access::{self, EnrollOutcome};
    // Gate on the durable grant (group membership), never on reachability:
    // a console user's presence grant makes the socket connectable, yet
    // they are exactly who runs this verb to gain SSH/remote access.
    if access::has_durable_access(socket) {
        writeln!(
            io::stdout(),
            "You already have durable TV control access (group member)."
        )?;
        return Ok(());
    }
    writeln!(
        io::stdout(),
        "Granting TV control access — authenticate as an administrator when prompted…"
    )?;
    match access::enable_access(socket)? {
        EnrollOutcome::Enrolled { user, group } => {
            writeln!(io::stdout(), "Added '{user}' to the '{group}' group.")?;
            writeln!(
                io::stdout(),
                "Log out and back in for it to take effect — a reboot is the most reliable, \
                 since the systemd user manager can outlive a logout and keep the old groups."
            )?;
        }
        // The durable gate read false a moment ago; only a race lands here.
        EnrollOutcome::AlreadyMember { .. } => {
            writeln!(io::stdout(), "You already have durable TV control access.")?;
        }
    }
    Ok(())
}

async fn run(cli: Cli) -> Result<()> {
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
    // reports it, `watch` is a passive view, the session-only commands
    // check the session service instead, and `reload` checks each service
    // it reloads. `user-profile` talks to both, so it checks both.
    if !matches!(
        cli.command,
        Command::Status
            | Command::Watch
            | Command::Profile(ProfileScope::User(_))
            | Command::Stream(_)
            | Command::Reload
    ) {
        check_release::<manager::Info>(&caller, "daemon").await?;
    }
    match cli.command {
        // Handled above, before the client opens (needs no connection).
        Command::EnableAccess => unreachable!(),
        // Dispatched by `main`, before any runtime exists.
        Command::Service(_) => unreachable!(),
        Command::Status => status(&session, &caller, site, &socket, json_out).await,
        Command::Reload => reload(&caller, cli.user_socket, site, json_out).await,
        Command::List => list(&session, &caller, site, json_out).await,
        Command::Watch => watch(&session, site).await,
        Command::Discover {
            vendor,
            watch: true,
            ..
        } => discover_session(&session, &caller, site, vendor, json_out).await,
        Command::Discover {
            vendor,
            timeout,
            watch: false,
        } => discover(&caller, vendor, timeout, json_out).await,
        Command::Pair {
            name,
            vendor,
            params,
            unmanaged,
        } => pair(&session, &caller, site, name, vendor, &params, !unmanaged).await,
        Command::Repair { tv } => {
            let started = call::<tv::RePair>(&caller, &tv, &()).await?;
            let _ = writeln!(io::stderr(), "re-pairing session {tv} started");
            watch_pairing(&session, &caller, site, tv, started.attempt).await
        }
        Command::Reconnect { tv } => ack::<tv::Reconnect>(&caller, &tv, &()).await,
        Command::Remove { tv } => ack::<tv::Remove>(&caller, &tv, &()).await,
        Command::Vendor { vendor: id } => vendor(&caller, id, json_out).await,
        Command::Power { tv, switch } => {
            attributes::power(&session, &caller, site, tv, switch, json_out).await
        }
        Command::Screen { tv, switch } => {
            attributes::screen(&session, &caller, site, tv, switch, json_out).await
        }
        Command::Mute { tv, switch } => {
            attributes::mute(&session, &caller, site, tv, switch, json_out).await
        }
        Command::Managed { tv, switch } => attributes::managed(&caller, tv, switch, json_out).await,
        Command::Volume { tv, level } => {
            attributes::volume(&session, &caller, site, tv, level, json_out).await
        }
        Command::Backlight { tv, level } => {
            attributes::backlight(&session, &caller, site, tv, level, json_out).await
        }
        Command::Input { tv, input } => {
            attributes::input(&session, &caller, site, tv, input, json_out).await
        }
        Command::PrimaryInput { tv, input, clear } => {
            attributes::primary_input(&caller, tv, input, clear, json_out).await
        }
        Command::Name { tv, name } => attributes::name(&caller, tv, name, json_out).await,
        Command::Config { tv, patch } => attributes::config_cmd(&caller, tv, patch, json_out).await,
        Command::SystemProfile {
            tv,
            profile,
            clear,
            effective,
        } => {
            let assignment = Assignment::new(profile, clear, effective);
            attributes::system_profile(&caller, tv, assignment, json_out).await
        }
        Command::UserProfile {
            tv,
            profile,
            clear,
            effective,
        } => {
            let (user, user_caller) = open_user(cli.user_socket, site).await?;
            let assignment = Assignment::new(profile, clear, effective);
            attributes::user_profile(&caller, &user, &user_caller, site, tv, assignment, json_out)
                .await
        }
        Command::Profile(ProfileScope::System(cmd)) => {
            profile_cmd(&session, &caller, site, cmd, json_out).await
        }
        Command::Profile(ProfileScope::User(cmd)) => {
            let (user, user_caller) = open_user(cli.user_socket, site).await?;
            user_profile_cmd(&user, &user_caller, site, cmd, json_out).await
        }
        Command::Stream(cmd) => {
            let (_user, user_caller) = open_user(cli.user_socket, site).await?;
            stream_cmd(&user_caller, cmd, json_out).await
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

/// Open the session service's user socket and check its release.
async fn open_user(user_socket: Option<PathBuf>, site: &str) -> Result<(Session, Caller)> {
    let user_socket =
        user_socket.unwrap_or_else(tisvlay_protocol::bootstrap::default_session_socket_path);
    let user = zenoh::open(tisvlay_protocol::bootstrap::client_config(&user_socket)?)
        .await
        .map_err(|e| {
            anyhow!(
                "session service unreachable at {}: {e}",
                user_socket.display()
            )
        })?;
    let user_caller = Caller::new(user.clone(), site);
    check_release::<user_profile::Info>(&user_caller, "session service").await?;
    Ok((user, user_caller))
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

/// One acknowledged rpc. Success prints nothing; a refusal is the error.
async fn ack<R: Unary<Response = rpc::Ack>>(
    caller: &Caller,
    target: &R::Target,
    request: &R::Request,
) -> Result<()> {
    call::<R>(caller, target, request).await?;
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
