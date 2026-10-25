//! Stream sources on the session service.

use anyhow::Result;
use tisvlay_cli::StreamCmd;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::user_stream;
use tisvlay_protocol::rpc;

use crate::call;
use crate::render::print_reply;

/// Arm/disarm a stream source (silent on success), or show the
/// current fact. Rides the user socket (the session service serves the
/// verb rpc there).
pub(crate) async fn stream_cmd(user_caller: &Caller, cmd: StreamCmd, json_out: bool) -> Result<()> {
    match cmd {
        StreamCmd::Start { source } => {
            let request = rpc::StreamSet {
                source: Some(source),
                active: true,
            };
            call::<user_stream::Set>(user_caller, &(), &request).await?;
            Ok(())
        }
        StreamCmd::Stop { source } => {
            let request = rpc::StreamSet {
                source: Some(source),
                active: false,
            };
            call::<user_stream::Set>(user_caller, &(), &request).await?;
            Ok(())
        }
        StreamCmd::Status => {
            let state = call::<user_stream::Status>(user_caller, &(), &()).await?;
            print_reply(&state, json_out)
        }
    }
}
