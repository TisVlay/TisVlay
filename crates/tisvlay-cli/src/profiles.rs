//! The system- and user-profile catalogs.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use tisvlay_cli::{SystemProfileCmd, UserProfileCmd, hydrate};
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::{profile, user_profile};
use tisvlay_protocol::rpc;
use zenoh::Session;

use crate::render::print_reply;
use crate::{ack, call};

/// How long a catalog read waits for the next profile before taking what
/// it has.
const CATALOG_QUIET: Duration = Duration::from_millis(600);

/// Print one profile catalog row.
fn print_profile(id: &str, body: &Value) -> io::Result<()> {
    write!(io::stdout(), "{}", format_profile(id, body))
}

fn format_profile(id: &str, body: &Value) -> String {
    let mut extras: Vec<String> = body
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| *key != "name")
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    extras.sort();
    format!(
        "{}  {}  {}",
        id,
        body.get("name").and_then(Value::as_str).unwrap_or("?"),
        extras.join(" "),
    ) + "\n"
}

/// Hydrate a cached catalog wildcard and print each body once the
/// stream goes quiet.
async fn list_catalog(session: &Session, keyexpr: &str, json_out: bool) -> Result<()> {
    let values = hydrate(session, keyexpr, CATALOG_QUIET, |_| false)
        .await
        .map_err(|e| anyhow!("subscribe catalog: {e}"))?;
    // The last key segment is the profile's UUID.
    let rows: BTreeMap<String, Value> = values
        .into_iter()
        .map(|(key, body)| (key.rsplit('/').next().unwrap_or_default().to_string(), body))
        .collect();
    if json_out {
        let out: serde_json::Map<String, Value> = rows.into_iter().collect();
        writeln!(
            io::stdout(),
            "{}",
            serde_json::to_string_pretty(&Value::Object(out))?
        )?;
        return Ok(());
    }
    if rows.is_empty() {
        writeln!(io::stdout(), "no profiles")?;
    }
    for (id, body) in rows {
        print_profile(&id, &body)?;
    }
    Ok(())
}

/// Build a profile body: `--body` merged over the positional name.
fn profile_body<T: serde::de::DeserializeOwned>(name: String, body: Option<String>) -> Result<T> {
    let mut body = match body {
        Some(body) => serde_json::from_str(&body)?,
        None => json!({}),
    };
    let Some(object) = body.as_object_mut() else {
        bail!("profile body must be a JSON object");
    };
    if object.get("name").is_none_or(Value::is_null) {
        object.insert("name".into(), Value::String(name));
    }
    serde_json::from_value(body).map_err(|e| anyhow!("invalid profile body: {e}"))
}

/// `profile system …`. `create` and `duplicate` print the new UUID
/// alone, so `id=$(…)` captures it.
pub(crate) async fn profile_cmd(
    session: &Session,
    caller: &Caller,
    site: &str,
    cmd: SystemProfileCmd,
    json_out: bool,
) -> Result<()> {
    match cmd {
        SystemProfileCmd::List => {
            let keyexpr = format!("tisvlay/{site}/profile/system/*");
            list_catalog(session, &keyexpr, json_out).await
        }
        SystemProfileCmd::Create { name, body } => {
            let request = rpc::ProfileCreate {
                id: None,
                body: profile_body(name, body)?,
            };
            let created = call::<profile::Create>(caller, &(), &request).await?;
            writeln!(io::stdout(), "{}", created.id)?;
            Ok(())
        }
        SystemProfileCmd::Update { id, body } => {
            let body = serde_json::from_str(&body)?;
            ack::<profile::Update>(caller, &(), &rpc::ProfileUpdate { id, body }).await
        }
        SystemProfileCmd::Duplicate { id, name } => {
            let created =
                call::<profile::Duplicate>(caller, &(), &rpc::ProfileDuplicate { id, name })
                    .await?;
            writeln!(io::stdout(), "{}", created.id)?;
            Ok(())
        }
        SystemProfileCmd::Delete { id } => {
            ack::<profile::Delete>(caller, &(), &rpc::TvRef { id }).await
        }
        SystemProfileCmd::Defaults => {
            let defaults = call::<profile::Defaults>(caller, &(), &()).await?;
            print_reply(&defaults, json_out)
        }
    }
}

/// `profile user …`, on the session service.
pub(crate) async fn user_profile_cmd(
    user: &Session,
    user_caller: &Caller,
    site: &str,
    cmd: UserProfileCmd,
    json_out: bool,
) -> Result<()> {
    match cmd {
        UserProfileCmd::List => {
            list_catalog(user, &format!("tisvlay/{site}/user/profile/*"), json_out).await
        }
        UserProfileCmd::Create { name, body } => {
            let request = rpc::UserProfileCreate {
                id: None,
                body: profile_body(name, body)?,
            };
            let created = call::<user_profile::Create>(user_caller, &(), &request).await?;
            writeln!(io::stdout(), "{}", created.id)?;
            Ok(())
        }
        UserProfileCmd::Update { id, body } => {
            let body = serde_json::from_str(&body)?;
            ack::<user_profile::Update>(user_caller, &(), &rpc::UserProfileUpdate { id, body })
                .await
        }
        UserProfileCmd::Duplicate { id, name } => {
            let request = rpc::ProfileDuplicate { id, name };
            let created = call::<user_profile::Duplicate>(user_caller, &(), &request).await?;
            writeln!(io::stdout(), "{}", created.id)?;
            Ok(())
        }
        UserProfileCmd::Delete { id } => {
            ack::<user_profile::Delete>(user_caller, &(), &rpc::TvRef { id }).await
        }
        UserProfileCmd::Defaults => {
            let defaults = call::<user_profile::Defaults>(user_caller, &(), &()).await?;
            print_reply(&defaults, json_out)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn format_profile_sorts_fields_and_handles_missing_or_non_object_names() {
        assert_eq!(
            super::format_profile(
                "uuid",
                &serde_json::json!({
                    "name": "Office",
                    "zeta": 2,
                    "alpha": 1
                })
            ),
            "uuid  Office  alpha=1 zeta=2\n"
        );
        assert_eq!(
            super::format_profile("uuid", &serde_json::json!({ "zeta": 2 })),
            "uuid  ?  zeta=2\n"
        );
        assert_eq!(
            super::format_profile("uuid", &serde_json::json!([])),
            "uuid  ?  \n"
        );
    }

    #[test]
    fn profile_body_fills_missing_name_and_preserves_explicit_body_fields() {
        let from_positional: serde_json::Value =
            super::profile_body("Office".into(), None).expect("valid positional profile");
        assert_eq!(from_positional, serde_json::json!({ "name": "Office" }));

        let from_body: serde_json::Value = super::profile_body(
            "Fallback".into(),
            Some(r#"{"name":"Studio","idle_timeout_seconds":900}"#.into()),
        )
        .expect("valid profile body");
        assert_eq!(from_body["name"], "Studio");
        assert_eq!(from_body["idle_timeout_seconds"], 900);

        let null_name: serde_json::Value = super::profile_body(
            "Fallback".into(),
            Some(r#"{"name":null,"on_sleep":null}"#.into()),
        )
        .expect("null name falls back to the positional name");
        assert_eq!(
            null_name,
            serde_json::json!({ "name": "Fallback", "on_sleep": null })
        );
    }

    #[test]
    fn profile_body_reports_json_and_typed_body_errors() {
        #[derive(Debug, serde::Deserialize)]
        struct NamedProfile {
            name: String,
        }

        let valid = super::profile_body::<NamedProfile>(
            "Fallback".into(),
            Some(r#"{"name":"Studio"}"#.into()),
        )
        .expect("a string profile name deserializes");
        assert_eq!(valid.name, "Studio");

        let malformed = super::profile_body::<serde_json::Value>("Office".into(), Some("{".into()));
        assert!(malformed.is_err());

        let non_object = super::profile_body::<NamedProfile>("Office".into(), Some("[]".into()))
            .expect_err("non-object JSON should return an error");
        assert_eq!(non_object.to_string(), "profile body must be a JSON object");

        let invalid_name =
            super::profile_body::<NamedProfile>("Office".into(), Some(r#"{"name":false}"#.into()))
                .expect_err("a non-string profile name is rejected");
        assert!(format!("{invalid_name:#}").contains("invalid profile body"));
    }
}
