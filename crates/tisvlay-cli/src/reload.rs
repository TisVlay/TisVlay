//! `reload`: apply hand edits of every service's state file.

use std::path::PathBuf;

use anyhow::{Result, bail};
use serde_json::{Value, json};
use tisvlay_protocol::caller::{Caller, RpcError};
use tisvlay_protocol::endpoint::{manager, user_profile, user_tray};
use tisvlay_protocol::rpc;

use crate::release_mismatch;

/// Reload every service's state file, as the GUI's Reload button does:
/// the daemon's, then the session's and the tray's over the user socket.
/// A service that isn't running has nothing to reload, so it's reported
/// rather than failed; any other error fails the command after the report.
pub(crate) async fn reload(
    caller: &Caller,
    user_socket: Option<PathBuf>,
    site: &str,
    json_out: bool,
) -> Result<()> {
    let daemon = match release_mismatch::<manager::Info>(caller, "daemon").await {
        Err(_) => ReloadOutcome::NotRunning,
        Ok(Some(mismatch)) => ReloadOutcome::Failed(mismatch),
        Ok(None) => reload_outcome(Some(caller.call::<manager::Reload>(&(), &()).await)),
    };
    let user_socket =
        user_socket.unwrap_or_else(tisvlay_protocol::bootstrap::default_session_socket_path);
    let user = match zenoh::open(tisvlay_protocol::bootstrap::client_config(&user_socket)?).await {
        Ok(user) => Some(Caller::new(user, site)),
        Err(_) => None,
    };
    let (session, tray) = match &user {
        Some(user) => (
            match release_mismatch::<user_profile::Info>(user, "session service").await {
                Err(_) => ReloadOutcome::NotRunning,
                Ok(Some(mismatch)) => ReloadOutcome::Failed(mismatch),
                Ok(None) => reload_outcome(Some(user.call::<user_profile::Reload>(&(), &()).await)),
            },
            match release_mismatch::<user_tray::Info>(user, "tray").await {
                Err(_) => ReloadOutcome::NotRunning,
                Ok(Some(mismatch)) => ReloadOutcome::Failed(mismatch),
                Ok(None) => reload_outcome(Some(user.call::<user_tray::Reload>(&(), &()).await)),
            },
        ),
        None => (ReloadOutcome::NotRunning, ReloadOutcome::NotRunning),
    };
    let outcomes = [("daemon", daemon), ("session", session), ("tray", tray)];
    if json_out {
        let report: serde_json::Map<String, Value> = outcomes
            .iter()
            .map(|(service, outcome)| ((*service).to_string(), outcome.to_json()))
            .collect();
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for (service, outcome) in &outcomes {
            println!("{:<8} {}", format!("{service}:"), outcome.describe());
        }
        // Each failure's report after the table: a file the service
        // couldn't parse reads like a compiler error, path included.
        for (service, outcome) in &outcomes {
            if let ReloadOutcome::Failed(report) = outcome {
                println!();
                if report.starts_with("error: ") {
                    println!("{report}");
                } else {
                    println!("error: {service}: {report}");
                }
            }
        }
    }
    let failed: Vec<&str> = outcomes
        .iter()
        .filter(|(_, outcome)| matches!(outcome, ReloadOutcome::Failed(_)))
        .map(|(service, _)| *service)
        .collect();
    if !failed.is_empty() {
        bail!("reload failed for {}", failed.join(", "));
    }
    Ok(())
}

/// One service's part of a [`reload`].
enum ReloadOutcome {
    Changed,
    Unchanged,
    NotRunning,
    Failed(String),
}

impl ReloadOutcome {
    fn describe(&self) -> String {
        match self {
            Self::Changed => "applied the file's edit".into(),
            Self::Unchanged => "the file is unchanged".into(),
            Self::NotRunning => "not running".into(),
            Self::Failed(_) => "failed".into(),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Changed => json!({ "status": "changed" }),
            Self::Unchanged => json!({ "status": "unchanged" }),
            Self::NotRunning => json!({ "status": "not-running" }),
            Self::Failed(error) => json!({ "status": "failed", "error": error }),
        }
    }
}

/// `None` (no user socket to reach) and an rpc no service answered both
/// mean the service isn't running.
fn reload_outcome(
    result: Option<std::result::Result<rpc::ReloadReport, RpcError>>,
) -> ReloadOutcome {
    match result {
        None | Some(Err(RpcError::Unavailable { .. })) => ReloadOutcome::NotRunning,
        Some(Ok(rpc::ReloadReport { changed: true })) => ReloadOutcome::Changed,
        Some(Ok(rpc::ReloadReport { changed: false })) => ReloadOutcome::Unchanged,
        Some(Err(RpcError::Service(e))) => ReloadOutcome::Failed(e.message),
        Some(Err(e)) => ReloadOutcome::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reload_outcome_maps_service_results_and_formats_them() {
        use super::{ReloadOutcome, reload_outcome};
        use tisvlay_protocol::caller::RpcError;
        use tisvlay_protocol::error::{ErrorCode, WireError};
        use tisvlay_protocol::rpc::ReloadReport;

        assert!(matches!(reload_outcome(None), ReloadOutcome::NotRunning));
        assert!(matches!(
            reload_outcome(Some(Err(RpcError::Unavailable {
                key: "reload".into(),
            }))),
            ReloadOutcome::NotRunning
        ));
        assert!(matches!(
            reload_outcome(Some(Ok(ReloadReport { changed: true }))),
            ReloadOutcome::Changed
        ));
        assert!(matches!(
            reload_outcome(Some(Ok(ReloadReport { changed: false }))),
            ReloadOutcome::Unchanged
        ));
        assert!(matches!(
            reload_outcome(Some(Err(RpcError::Service(WireError {
                code: ErrorCode::Internal,
                message: "invalid TOML".into(),
            })))),
            ReloadOutcome::Failed(error) if error == "invalid TOML"
        ));
        assert!(matches!(
            reload_outcome(Some(Err(RpcError::Timeout {
                key: "reload".into(),
            }))),
            ReloadOutcome::Failed(_)
        ));

        let outcomes = [
            (
                ReloadOutcome::Changed,
                "applied the file's edit",
                serde_json::json!({ "status": "changed" }),
            ),
            (
                ReloadOutcome::Unchanged,
                "the file is unchanged",
                serde_json::json!({ "status": "unchanged" }),
            ),
            (
                ReloadOutcome::NotRunning,
                "not running",
                serde_json::json!({ "status": "not-running" }),
            ),
            (
                ReloadOutcome::Failed("invalid TOML".into()),
                "failed",
                serde_json::json!({ "status": "failed", "error": "invalid TOML" }),
            ),
        ];
        for (outcome, description, json) in outcomes {
            assert_eq!(outcome.describe(), description);
            assert_eq!(outcome.to_json(), json);
        }
    }
}
