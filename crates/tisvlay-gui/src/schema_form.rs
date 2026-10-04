//! Shared infrastructure for schema-driven config forms.
//!
//! The pairing wizard's step-2 form was the first consumer; the
//! TVs-pane edit-mode toggle is the second. Both render a list of
//! `PairingField`s into a Slint `[PairingFieldRow]` model and run
//! the same per-row validation lifecycle. This module holds the
//! pure-Rust pieces that have no dependence on the wizard's
//! transient state — the rendering live in
//! `ui/components/schema_form.slint`.
//!
//! Bridge to Slint: [`AutofilledRow`] is a `Send` plain-data
//! carrier; helpers building the model from a worker thread accumulate
//! `Vec<AutofilledRow>` and convert via [`AutofilledRow::into_slint`]
//! on the event-loop thread (where `ModelRc::new` is allowed).
//!
//! Wire format: `field-type` strings are kebab-case, matching
//! `tisvlay_core::pairing_schema::PairingFieldType::as_str()`.

use std::collections::HashMap;

use crate::compat::{PairingField, PairingFieldType};
use slint::{Model, ModelRc, SharedString, VecModel};

use crate::PairingFieldRow;

/// Plain-data carrier for an autofilled schema row.
///
/// `Send` (unlike `PairingFieldRow`, whose `options: ModelRc<…>`
/// holds a non-`Send` `Rc`), so workers can build the autofilled
/// list off the Slint thread. The conversion to `PairingFieldRow`
/// happens inside an `on_ui` closure once we're back on the event-loop
/// thread.
#[derive(Clone, Debug, Default)]
pub(crate) struct AutofilledRow {
    pub name: String,
    pub label: String,
    pub helper: String,
    pub field_type: String,
    pub required: bool,
    pub placeholder: String,
    pub options: Vec<String>,
    pub value: String,
    /// True when `value` came from a real autofill source (SSDP
    /// `vendor_data` entry, manual-entry MAC enrichment, or the
    /// address fallback for `ip`/`address` fields) rather than the
    /// schema's static `default`. Drives the "Auto-detected" badge
    /// in `SchemaForm`; flipped to false on user edit by the
    /// `field-value-edited` handler in `lib.rs`.
    pub autofilled: bool,
}

impl AutofilledRow {
    pub fn into_slint(self) -> PairingFieldRow {
        let (state, error) = validate_value(
            &self.name,
            parse_field_type(&self.field_type),
            self.required,
            &self.options,
            &self.value,
        );
        PairingFieldRow {
            name: SharedString::from(self.name),
            label: SharedString::from(self.label),
            helper: SharedString::from(self.helper),
            field_type: SharedString::from(self.field_type),
            required: self.required,
            placeholder: SharedString::from(self.placeholder),
            options: ModelRc::new(VecModel::from(
                self.options
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            )),
            value: SharedString::from(self.value),
            state: SharedString::from(state),
            error: SharedString::from(error),
            autofilled: self.autofilled,
        }
    }
}

/// Resolve a kebab-case `field-type` wire string back to its typed
/// [`PairingFieldType`]. Falls back to `String` for unknown values
/// — keeps the form rendering through a forward-compat schema that
/// introduces a new variant before we've taught the GUI about it.
/// The validator just treats it as plain text.
pub fn parse_field_type(s: &str) -> PairingFieldType {
    match s {
        "password" => PairingFieldType::Password,
        "ip" => PairingFieldType::Ip,
        "mac" => PairingFieldType::Mac,
        "port" => PairingFieldType::Port,
        "int" => PairingFieldType::Int,
        "bool" => PairingFieldType::Bool,
        "select" => PairingFieldType::Select,
        "path" => PairingFieldType::Path,
        _ => PairingFieldType::String,
    }
}

/// Compute `(state, error)` for one row's `value` by reusing the
/// daemon's canonical [`PairingField::validate`] — same checks the
/// daemon will run server-side, so a value that passes here also
/// passes the `pair` / `update-config` rpcs. Empty +
/// non-required → "pristine" (no opinion yet); otherwise validation
/// routes us into "valid" or "invalid".
pub fn validate_value(
    name: &str,
    field_type: PairingFieldType,
    required: bool,
    options: &[String],
    value: &str,
) -> (&'static str, String) {
    if value.is_empty() && !required {
        return ("pristine", String::new());
    }
    let field = PairingField {
        name: name.to_string(),
        label: String::new(),
        helper: String::new(),
        translation_key: None,
        field_type,
        required,
        placeholder: String::new(),
        default: String::new(),
        autofill: String::new(),
        options: options.to_vec(),
        validation: None,
        visibility: Default::default(),
    };
    match field.validate(value) {
        Ok(()) => ("valid", String::new()),
        Err(tisvlay_core::errors::ConfigError::InvalidVendorConfig { reason, .. }) => {
            ("invalid", reason)
        }
        Err(other) => ("invalid", other.to_string()),
    }
}

/// Walk the schema-fields model and decide whether the form's
/// submit button should enable. True iff every required field has a
/// non-empty value AND no field is in `state == "invalid"`.
pub fn compute_schema_valid(model: &ModelRc<PairingFieldRow>) -> bool {
    for i in 0..model.row_count() {
        let Some(row) = model.row_data(i) else {
            continue;
        };
        if row.state == "invalid" {
            return false;
        }
        if row.required && row.value.is_empty() {
            return false;
        }
    }
    true
}

/// Build an [`AutofilledRow`] for each schema field, applying
/// autofill from the supplied `vendor_data` map (and `address` as a
/// default for the conventional `ip` / `address` fields — SSDP gives
/// us an IP and the daemon-side `LgVendorConfig` accepts `address`
/// as an alias for `ip`, so seeding either works).
///
/// Autofill rule: a field's `autofill = "discovery.<key>"` resolves
/// to `vendor_data[<key>]`. Missing entries fall back to the
/// field's `default`. The user can still edit the pre-filled value
/// before submitting.
///
/// Decoupled from any specific carrier struct (e.g. the wizard's
/// `WizardEntry`) so the same routine drives both the pairing
/// wizard and the GUI's edit-pane prefill.
pub(crate) fn autofill_schema(
    schema: &[PairingField],
    vendor_data: &HashMap<String, String>,
    address: &str,
) -> Vec<AutofilledRow> {
    schema
        .iter()
        .enumerate()
        .map(|(index, f)| {
            let mut value = f.default.clone();
            // Track whether the value came from a real autofill
            // source (not the schema's static default). The trust
            // badge in `SchemaForm` rides on this so the user can
            // tell what the system populated vs what they typed.
            let mut autofilled = false;
            if let Some(key) = f.autofill.strip_prefix("discovery.") {
                if let Some(v) = vendor_data.get(key) {
                    value = v.clone();
                    autofilled = !value.is_empty();
                } else if (key == "ip" || key == "address") && !address.is_empty() {
                    value = address.to_string();
                    autofilled = true;
                }
            }
            // Conventional fallback: the schema's FIRST field is the
            // vendor's primary descriptor and `address` is the value
            // the user just picked or typed for exactly that field —
            // never make them retype it. Fields literally named
            // "ip" / "address" keep the same treatment at any position.
            if value.is_empty()
                && (index == 0 || f.name == "ip" || f.name == "address")
                && !address.is_empty()
            {
                value = address.to_string();
                autofilled = true;
            }
            AutofilledRow {
                name: f.name.clone(),
                label: f.label.clone(),
                helper: f.helper.clone(),
                field_type: f.field_type.as_str().to_string(),
                required: f.required,
                placeholder: f.placeholder.clone(),
                options: f.options.clone(),
                value,
                autofilled,
            }
        })
        .collect()
}

/// Build an [`AutofilledRow`] list by projecting current values from
/// a `get-config` rpc JSON payload through a schema.
///
/// Mirrors [`autofill_schema`] but the value source is the daemon's
/// existing config rather than a freshly-discovered TV: each
/// schema field looks itself up in the supplied object by `name`,
/// falling back to the field's `default` when absent. Used by the
/// TVs pane's edit-pencil flow to prefill the form with the TV's
/// current values before the user makes any changes.
///
/// The `get-config` reply nests the vendor table under `config`
/// (top level carries the TV-level fields: `name`, `managed`, …);
/// the schema fields live in that nested table, so it wins when
/// present. A flat object still works — callers can pass the vendor
/// table directly.
pub(crate) fn prefill_schema_from_config(
    schema: &[PairingField],
    config: &serde_json::Value,
) -> Vec<AutofilledRow> {
    let table = config
        .get("config")
        .and_then(|nested| nested.as_object())
        .or_else(|| config.as_object());
    schema
        .iter()
        .map(|f| {
            // Coerce the JSON scalar to a string for the form's
            // `value` slot. Numbers/bools get their natural string
            // repr so an `int`/`port`/`bool` field round-trips
            // cleanly through the same widget that handles user
            // input. Missing or null falls through to the field's
            // declared default.
            let value = table
                .and_then(|t| t.get(&f.name))
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| f.default.clone());
            AutofilledRow {
                name: f.name.clone(),
                label: f.label.clone(),
                helper: f.helper.clone(),
                field_type: f.field_type.as_str().to_string(),
                required: f.required,
                placeholder: f.placeholder.clone(),
                options: f.options.clone(),
                value,
                // Prefilled-from-stored-config values are not
                // "auto-detected" in the wizard sense — they're what
                // the user already saved. No badge.
                autofilled: false,
            }
        })
        .collect()
}

/// What a cleared (empty) optional row becomes in the config payload.
///
/// Both variants mean "reset to the schema default" — the difference is
/// purely which vocabulary the receiving rpc speaks. This is what makes
/// clearing a field mean the same thing on both form paths; only the
/// wire encoding differs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ClearedOptionals {
    /// Pairing payload: omit the key. A fresh config parse applies the
    /// serde default for every absent field, and the pairing params
    /// become TOML, which cannot carry null anyway.
    Omit,
    /// Edit patch: emit an explicit `null`. The daemon's `update-config`
    /// merge removes the stored key on null, so the serde default
    /// reapplies on the next parse. Omitting instead would silently keep
    /// the old stored value — "clear" would mean nothing on edit.
    Null,
}

/// Build a config-payload JSON object from the schema-fields model.
/// Walks each row and emits `{ <name>: <value>, ... }` matching the
/// schema's `name` keys; a cleared optional row is encoded per
/// [`ClearedOptionals`] (an empty `""` is never sent — typed validators
/// treat absent and empty differently). The daemon then validates each
/// value server-side via the `pair` rpc (pairing) or the
/// `update-config` rpc (edit).
///
/// Bool fields convert to JSON booleans; numeric fields (port /
/// int) convert to JSON numbers when parseable, falling back to
/// string. Strings (including ip / mac / password / path) stay
/// strings.
pub fn build_config_json_from_schema(
    model: &ModelRc<PairingFieldRow>,
    cleared: ClearedOptionals,
) -> String {
    let mut obj = serde_json::Map::new();
    for i in 0..model.row_count() {
        let Some(row) = model.row_data(i) else {
            continue;
        };
        let value = row.value.to_string();
        // A cleared optional field means "reset to the schema default":
        // absent on pair, explicit null on edit. (Required fields can't
        // be empty here — the submit gate blocks them.)
        if value.is_empty() && !row.required {
            if cleared == ClearedOptionals::Null {
                obj.insert(row.name.to_string(), serde_json::Value::Null);
            }
            continue;
        }
        let json_val = match row.field_type.as_str() {
            "bool" => serde_json::Value::Bool(value == "true"),
            "port" | "int" => value
                .parse::<i64>()
                .map(serde_json::Value::from)
                .unwrap_or_else(|_| serde_json::Value::String(value)),
            _ => serde_json::Value::String(value),
        };
        obj.insert(row.name.to_string(), json_val);
    }
    serde_json::Value::Object(obj).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, field_type: PairingFieldType, required: bool) -> PairingField {
        PairingField {
            name: name.into(),
            field_type,
            required,
            ..Default::default()
        }
    }

    fn row(
        name: &str,
        field_type: &str,
        required: bool,
        value: &str,
        state: &str,
    ) -> PairingFieldRow {
        PairingFieldRow {
            name: SharedString::from(name),
            label: SharedString::default(),
            helper: SharedString::default(),
            field_type: SharedString::from(field_type),
            required,
            placeholder: SharedString::default(),
            options: ModelRc::new(VecModel::<SharedString>::default()),
            value: SharedString::from(value),
            state: SharedString::from(state),
            error: SharedString::default(),
            autofilled: false,
        }
    }

    fn model(rows: Vec<PairingFieldRow>) -> ModelRc<PairingFieldRow> {
        ModelRc::new(VecModel::from(rows))
    }

    // ---------- parse_field_type ----------

    #[test]
    fn parse_field_type_maps_known_kebab_strings() {
        for (input, expected) in [
            ("password", PairingFieldType::Password),
            ("ip", PairingFieldType::Ip),
            ("mac", PairingFieldType::Mac),
            ("port", PairingFieldType::Port),
            ("int", PairingFieldType::Int),
            ("bool", PairingFieldType::Bool),
            ("select", PairingFieldType::Select),
            ("path", PairingFieldType::Path),
        ] {
            assert_eq!(parse_field_type(input), expected, "input={input}");
        }
    }

    #[test]
    fn parse_field_type_falls_back_to_string_for_unknown() {
        // Forward-compat: a future schema variant the GUI hasn't
        // learned about yet still renders, treated as plain text.
        assert_eq!(parse_field_type("future-field"), PairingFieldType::String);
        assert_eq!(parse_field_type(""), PairingFieldType::String);
    }

    // ---------- validate_value ----------

    #[test]
    fn validate_value_pristine_when_empty_and_optional() {
        let (state, error) = validate_value("ip", PairingFieldType::Ip, false, &[], "");
        assert_eq!(state, "pristine");
        assert!(error.is_empty());
    }

    #[test]
    fn validate_value_invalid_when_empty_and_required() {
        let (state, _err) = validate_value("ip", PairingFieldType::Ip, true, &[], "");
        assert_eq!(state, "invalid");
    }

    #[test]
    fn validate_value_routes_typed_errors_through_invalid_vendor_config() {
        let (state, error) = validate_value("ip", PairingFieldType::Ip, true, &[], "not-an-ip");
        assert_eq!(state, "invalid");
        assert!(!error.is_empty(), "should surface a reason");
    }

    #[test]
    fn validate_value_passes_well_formed_inputs() {
        let cases = [
            ("ip", PairingFieldType::Ip, "192.168.1.10"),
            ("mac", PairingFieldType::Mac, "AA:BB:CC:DD:EE:FF"),
            ("port", PairingFieldType::Port, "3000"),
            ("int", PairingFieldType::Int, "42"),
            ("bool", PairingFieldType::Bool, "true"),
        ];
        for (name, ty, value) in cases {
            let (state, _err) = validate_value(name, ty, true, &[], value);
            assert_eq!(state, "valid", "{name}={value} should validate");
        }
    }

    #[test]
    fn validate_value_select_must_match_an_option() {
        let opts = vec!["wss".into(), "ws".into()];
        let (ok, _) = validate_value("scheme", PairingFieldType::Select, true, &opts, "wss");
        assert_eq!(ok, "valid");
        let (bad, _) = validate_value("scheme", PairingFieldType::Select, true, &opts, "smtp");
        assert_eq!(bad, "invalid");
    }

    // ---------- autofill_schema ----------

    #[test]
    fn autofill_schema_uses_field_default_without_directives() {
        let mut f = field("name", PairingFieldType::String, false);
        f.default = "Living Room".into();
        let rows = autofill_schema(&[f], &HashMap::new(), "");
        assert_eq!(rows[0].value, "Living Room");
    }

    #[test]
    fn autofill_schema_resolves_discovery_keys() {
        let mut f = field("scheme", PairingFieldType::String, false);
        f.autofill = "discovery.scheme".into();
        let vendor = HashMap::from([("scheme".to_string(), "wss".to_string())]);
        let rows = autofill_schema(&[f], &vendor, "");
        assert_eq!(rows[0].value, "wss");
    }

    #[test]
    fn autofill_schema_falls_back_to_address_for_discovery_ip_alias() {
        // `autofill = "discovery.ip"` with empty vendor_data uses the
        // discovered SSDP address. Same for `discovery.address`.
        let mut f = field("ip", PairingFieldType::Ip, true);
        f.autofill = "discovery.ip".into();
        let rows = autofill_schema(&[f], &HashMap::new(), "192.168.1.10");
        assert_eq!(rows[0].value, "192.168.1.10");
    }

    #[test]
    fn autofill_schema_seeds_address_for_conventionally_named_fields() {
        // Field literally named "ip" / "address" gets the SSDP address
        // even without an explicit `autofill` directive.
        let f = field("ip", PairingFieldType::Ip, true);
        let rows = autofill_schema(&[f], &HashMap::new(), "192.168.1.10");
        assert_eq!(rows[0].value, "192.168.1.10");
    }

    #[test]
    fn autofill_schema_leaves_value_empty_when_no_source_matches() {
        // The first field is the vendor's primary descriptor and
        // always receives the supplied address; a later field with no
        // autofill source stays empty.
        let first = field("device", PairingFieldType::Path, true);
        let f = field("port", PairingFieldType::Port, false);
        let rows = autofill_schema(&[first, f], &HashMap::new(), "/dev/cec0");
        assert_eq!(rows[0].value, "/dev/cec0");
        assert!(rows[0].autofilled);
        assert_eq!(rows[1].value, "");
    }

    // ---------- prefill_schema_from_config ----------

    #[test]
    fn prefill_schema_from_config_projects_string_values() {
        let f = field("ip", PairingFieldType::Ip, true);
        let cfg = serde_json::json!({ "ip": "10.0.0.5" });
        let rows = prefill_schema_from_config(&[f], &cfg);
        assert_eq!(rows[0].value, "10.0.0.5");
    }

    #[test]
    fn prefill_schema_from_config_reads_the_nested_vendor_table() {
        // The full `get-config` reply shape: TV-level fields at the
        // top, vendor fields nested under `config`. The prefill must
        // read the nested table — otherwise every field misses and
        // the edit pane opens blank over a fully-configured TV.
        let ip = field("ip", PairingFieldType::Ip, true);
        let mac = field("mac", PairingFieldType::Mac, false);
        let reply = serde_json::json!({
            "name": "Living Room",
            "managed": true,
            "config": { "ip": "192.168.1.50", "mac": "84:90:0a:63:a9:94" }
        });
        let rows = prefill_schema_from_config(&[ip, mac], &reply);
        assert_eq!(rows[0].value, "192.168.1.50");
        assert_eq!(rows[1].value, "84:90:0a:63:a9:94");
    }

    #[test]
    fn prefill_schema_from_config_stringifies_numbers_and_bools() {
        let port = field("port", PairingFieldType::Port, false);
        let auto = field("automated", PairingFieldType::Bool, false);
        let cfg = serde_json::json!({ "port": 3001, "automated": true });
        let rows = prefill_schema_from_config(&[port, auto], &cfg);
        assert_eq!(rows[0].value, "3001");
        assert_eq!(rows[1].value, "true");
    }

    #[test]
    fn prefill_schema_from_config_falls_back_to_default_for_missing_or_empty() {
        let mut f = field("ip", PairingFieldType::Ip, false);
        f.default = "0.0.0.0".into();
        let missing = serde_json::json!({});
        let rows = prefill_schema_from_config(&[f.clone()], &missing);
        assert_eq!(rows[0].value, "0.0.0.0");

        let empty = serde_json::json!({ "ip": "" });
        let rows = prefill_schema_from_config(&[f.clone()], &empty);
        assert_eq!(rows[0].value, "0.0.0.0");

        let null = serde_json::json!({ "ip": null });
        let rows = prefill_schema_from_config(&[f], &null);
        assert_eq!(rows[0].value, "0.0.0.0");
    }

    // ---------- compute_schema_valid ----------

    #[test]
    fn compute_schema_valid_all_valid_rows_return_true() {
        let m = model(vec![
            row("ip", "ip", true, "10.0.0.1", "valid"),
            row("port", "port", false, "3001", "valid"),
        ]);
        assert!(compute_schema_valid(&m));
    }

    #[test]
    fn compute_schema_valid_required_empty_returns_false() {
        let m = model(vec![row("ip", "ip", true, "", "pristine")]);
        assert!(!compute_schema_valid(&m));
    }

    #[test]
    fn compute_schema_valid_optional_empty_returns_true() {
        let m = model(vec![row("port", "port", false, "", "pristine")]);
        assert!(compute_schema_valid(&m));
    }

    #[test]
    fn compute_schema_valid_invalid_state_returns_false() {
        let m = model(vec![row("ip", "ip", true, "not-an-ip", "invalid")]);
        assert!(!compute_schema_valid(&m));
    }

    // ---------- build_config_json_from_schema ----------

    #[test]
    fn build_config_json_omits_cleared_optionals_for_pair() {
        let m = model(vec![
            row("ip", "ip", true, "10.0.0.1", "valid"),
            row("name", "string", false, "", "pristine"),
        ]);
        let json = build_config_json_from_schema(&m, ClearedOptionals::Omit);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ip"], "10.0.0.1");
        assert!(
            parsed.get("name").is_none(),
            "pair payload omits a cleared optional (absent → serde default)"
        );
    }

    #[test]
    fn build_config_json_nulls_cleared_optionals_for_edit() {
        // The edit patch must carry an explicit null for a cleared
        // optional — the merge removes the stored key on null, so the
        // serde default reapplies. Omitting would keep the old value
        // (the "cleared MAC silently persists" bug).
        let m = model(vec![
            row("ip", "ip", true, "10.0.0.1", "valid"),
            row("mac", "mac", false, "", "pristine"),
        ]);
        let json = build_config_json_from_schema(&m, ClearedOptionals::Null);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ip"], "10.0.0.1");
        assert_eq!(
            parsed.get("mac"),
            Some(&serde_json::Value::Null),
            "edit patch nulls a cleared optional (remove → serde default)"
        );
    }

    #[test]
    fn build_config_json_emits_typed_scalars_for_bool_and_numeric() {
        let m = model(vec![
            row("automated", "bool", false, "true", "valid"),
            row("port", "port", false, "3001", "valid"),
            row("retries", "int", false, "5", "valid"),
            row("ip", "ip", true, "10.0.0.1", "valid"),
        ]);
        let parsed: serde_json::Value =
            serde_json::from_str(&build_config_json_from_schema(&m, ClearedOptionals::Omit))
                .unwrap();
        assert_eq!(parsed["automated"], serde_json::Value::Bool(true));
        assert_eq!(parsed["port"].as_i64(), Some(3001));
        assert_eq!(parsed["retries"].as_i64(), Some(5));
        assert_eq!(parsed["ip"], serde_json::Value::String("10.0.0.1".into()));
    }

    #[test]
    fn build_config_json_falls_back_to_string_for_unparseable_numerics() {
        let m = model(vec![row("port", "port", false, "not-a-number", "invalid")]);
        let parsed: serde_json::Value =
            serde_json::from_str(&build_config_json_from_schema(&m, ClearedOptionals::Omit))
                .unwrap();
        assert_eq!(
            parsed["port"],
            serde_json::Value::String("not-a-number".into())
        );
    }
}
