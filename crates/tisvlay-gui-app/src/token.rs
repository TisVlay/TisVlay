//! Enum tokens: the serde names the GUI's rows carry for core enums (a
//! profile action, a pairing field's type), which the rpcs round-trip.
//! Serde is their one definition, so no hand-written table can drift.

use serde::Serialize;
use serde::de::value::{Error, StrDeserializer};
use serde::de::{DeserializeOwned, IntoDeserializer as _};

/// An enum's token: its serde name.
pub(crate) fn enum_token<T: Serialize>(value: T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(token)) => token,
        other => unreachable!("token enums serialize as a unit-variant string, got {other:?}"),
    }
}

/// Parse a token back into its enum by serde name; an unknown token's
/// error names it and the expected ones.
pub(crate) fn parse_token<T: DeserializeOwned>(token: &str) -> anyhow::Result<T> {
    let deserializer: StrDeserializer<'_, Error> = token.into_deserializer();
    Ok(T::deserialize(deserializer)?)
}
