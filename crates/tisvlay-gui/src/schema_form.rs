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

use slint::{Model, ModelRc, SharedString, VecModel};
use tisvlay_core::{PairingField, PairingFieldType};

use crate::PairingFieldRow;

/// Plain-data carrier for an autofilled schema row.
///
/// `Send` (unlike `PairingFieldRow`, whose `options: ModelRc<…>`
/// holds a non-`Send` `Rc`), so workers can build the autofilled
/// list off the Slint thread. The conversion to `PairingFieldRow`
/// happens inside an `on_ui` closure once we're back on the event-loop
/// thread.
#[derive(Clone, Debug)]
pub(crate) struct AutofilledRow {
    pub name: String,
    pub label: String,
    pub helper: String,
    pub field_type: String,
    pub required: bool,
    pub placeholder: String,
    pub options: Vec<String>,
    pub value: String,
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
/// passes in `Manager1.AddTv` / `Manager1.UpdateTvConfig`. Empty +
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
/// Autofill rule mirrors plan5: a field's `autofill = "discovery.<key>"`
/// resolves to `vendor_data[<key>]`. Missing entries fall back to
/// the field's `default`. The user can still edit the pre-filled
/// value before submitting.
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
        .map(|f| {
            let mut value = f.default.clone();
            if let Some(key) = f.autofill.strip_prefix("discovery.") {
                if let Some(v) = vendor_data.get(key) {
                    value = v.clone();
                } else if (key == "ip" || key == "address") && !address.is_empty() {
                    value = address.to_string();
                }
            }
            // Conventional fallback: a field literally named "ip" /
            // "address" gets the supplied address even without an
            // explicit `autofill` directive — IP-addressed backends
            // discovered via SSDP should never make the user retype
            // what we just received from the device.
            if value.is_empty() && (f.name == "ip" || f.name == "address") {
                value = address.to_string();
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
            }
        })
        .collect()
}

/// Build a config-payload JSON object from the schema-fields model.
/// Walks each row, drops empty optional fields, and emits
/// `{ <name>: <value>, ... }` matching the schema's `name` keys.
/// The daemon then validates each value server-side via
/// `Manager1.AddTv` (pairing) or `Manager1.UpdateTvConfig` (edit).
///
/// Bool fields convert to JSON booleans; numeric fields (port /
/// int) convert to JSON numbers when parseable, falling back to
/// string. Strings (including ip / mac / password / path) stay
/// strings.
pub fn build_config_json_from_schema(model: &ModelRc<PairingFieldRow>) -> String {
    let mut obj = serde_json::Map::new();
    for i in 0..model.row_count() {
        let Some(row) = model.row_data(i) else {
            continue;
        };
        let value = row.value.to_string();
        // Skip empty optional fields so the daemon's schema
        // validator treats them as absent (not as "" — that's a
        // typed required-field failure for required fields).
        if value.is_empty() && !row.required {
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
