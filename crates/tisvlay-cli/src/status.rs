//! `status`: the daemon's liveliness and release.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::json;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::manager;
use tisvlay_protocol::keys;
use zenoh::Session;

pub(crate) async fn status(
    session: &Session,
    caller: &Caller,
    site: &str,
    socket: &Path,
    json_out: bool,
) -> Result<()> {
    let replies = session
        .liveliness()
        .get(keys::daemon_alive(site).as_str())
        .timeout(Duration::from_secs(2))
        .await
        .map_err(|e| anyhow!("liveliness get: {e}"))?;
    let alive = matches!(replies.recv_async().await, Ok(reply) if reply.result().is_ok());
    let release = match alive {
        true => caller
            .call::<manager::Info>(&(), &())
            .await
            .ok()
            .map(|info| info.version),
        false => None,
    };
    let compatible = release
        .as_deref()
        .map(|release| tisvlay_protocol::compatible(tisvlay_protocol::VERSION, release));
    if json_out {
        println!(
            "{}",
            json!({
                "alive": alive,
                "site": site,
                "socket": socket,
                "release": release,
                "compatible": compatible,
            })
        );
    }
    let healthy = alive && compatible != Some(false);
    if json_out {
        if !healthy {
            #[expect(
                clippy::exit,
                reason = "`status --json` reports a missing or incompatible daemon as exit code 1 after printing its JSON, with no error text"
            )]
            std::process::exit(1);
        }
        return Ok(());
    }
    if alive {
        println!("daemon alive: site '{site}' at {}", socket.display());
        if let Some(release) = &release {
            println!(
                "daemon release: {release} (this CLI: {})",
                tisvlay_protocol::VERSION
            );
        }
        if compatible == Some(false) {
            bail!(
                "the daemon runs release {}, which this CLI ({}) can't talk to; restart it or upgrade so they match",
                release.as_deref().unwrap_or_default(),
                tisvlay_protocol::VERSION
            );
        }
        Ok(())
    } else {
        bail!(
            "no daemon liveliness token for site '{site}' at {}",
            socket.display()
        );
    }
}
