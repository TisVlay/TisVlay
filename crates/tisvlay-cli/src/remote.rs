//! Remote-session sources on the session service.

use anyhow::Result;
use tisvlay_cli::RemoteSessionCmd;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::user_remote;
use tisvlay_protocol::rpc;

use crate::call;
use crate::render::print_reply;

/// Arm/disarm a remote-session source, or show the current fact. Rides
/// the user socket (the session service serves the verb rpc there).
pub(crate) async fn remote_session_cmd(
    user_caller: &Caller,
    cmd: RemoteSessionCmd,
    json_out: bool,
) -> Result<()> {
    match cmd {
        RemoteSessionCmd::Start { source } => {
            let request = rpc::RemoteSet {
                source: Some(source.clone()),
                active: true,
            };
            call::<user_remote::Set>(user_caller, &(), &request).await?;
            if !json_out {
                println!("armed source '{source}'");
            }
            Ok(())
        }
        RemoteSessionCmd::Stop { source } => {
            let request = rpc::RemoteSet {
                source: Some(source.clone()),
                active: false,
            };
            call::<user_remote::Set>(user_caller, &(), &request).await?;
            if !json_out {
                println!("disarmed source '{source}'");
            }
            Ok(())
        }
        RemoteSessionCmd::Status => {
            let state = call::<user_remote::Status>(user_caller, &(), &()).await?;
            print_reply(&state, json_out)
        }
    }
}
