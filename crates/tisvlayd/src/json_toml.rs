//! Lossless JSON ↔ TOML value conversion shared between the
//! daemon's D-Bus surface (which speaks JSON-strings on the wire)
//! and the in-memory `TvConfig.vendor_config` map (which is keyed
//! by TOML so it can serialize directly to the on-disk config).
//!
//! The conversion preserves scalars (string/number/bool), arrays,
//! and tables. JSON `null` is rejected — TOML has no null and
//! treating a JSON-null field as "delete this key" would conflate
//! semantics with "leave the field absent." Callers should drop
//! null-valued keys from the JSON before calling.

/// Convert a JSON value to a TOML value.
///
/// Used by `Manager1.AddTv` (config_json → vendor_config) and
/// `Tv1.UpdateConfig` (json_patch → vendor patches merged into
/// existing `vendor_config`).
pub(crate) fn json_to_toml(json: &serde_json::Value) -> Result<toml::Value, String> {
    match json {
        serde_json::Value::Null => {
            Err("JSON null values are not supported in TOML config".to_string())
        }
        serde_json::Value::Bool(b) => Ok(toml::Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(toml::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(toml::Value::Float(f))
            } else {
                Err("Unsupported number type".to_string())
            }
        }
        serde_json::Value::String(s) => Ok(toml::Value::String(s.clone())),
        serde_json::Value::Array(arr) => {
            let values: Result<Vec<toml::Value>, String> = arr.iter().map(json_to_toml).collect();
            Ok(toml::Value::Array(values?))
        }
        serde_json::Value::Object(obj) => {
            let mut table = toml::map::Map::new();
            for (key, value) in obj {
                table.insert(key.clone(), json_to_toml(value)?);
            }
            Ok(toml::Value::Table(table))
        }
    }
}

/// Convert a TOML value to a JSON value.
///
/// Used by `Tv1.GetConfig` to project the in-memory
/// `TvConfig.vendor_config` back out as a JSON payload for the
/// edit form to prefill from. Datetime values lose precision
/// (rendered as ISO-8601 strings) — none of our backends use
/// datetime fields today, so this is fine and we'll deal with
/// it when a backend genuinely needs round-trip datetime.
pub(crate) fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => serde_json::Value::Object(
            table
                .iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_round_trips() {
        for json in [
            serde_json::json!("hi"),
            serde_json::json!(42),
            serde_json::json!(true),
            serde_json::json!(false),
        ] {
            let toml = json_to_toml(&json).unwrap();
            assert_eq!(toml_to_json(&toml), json);
        }
    }

    #[test]
    fn nested_object_round_trips() {
        let json = serde_json::json!({
            "ip": "192.168.1.100",
            "port": 3001,
            "options": ["a", "b"],
            "nested": { "key": "value" }
        });
        let toml = json_to_toml(&json).unwrap();
        assert_eq!(toml_to_json(&toml), json);
    }

    #[test]
    fn json_null_is_rejected() {
        let json = serde_json::json!({ "field": null });
        assert!(json_to_toml(&json).is_err());
    }
}
