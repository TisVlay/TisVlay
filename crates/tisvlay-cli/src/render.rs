//! Human and `--json` rendering shared by the read verbs.

use std::io::{self, Write};

use anyhow::Result;
use serde_json::Value;

/// Render a flat JSON object as `key = value` lines (nested objects
/// indent), sorted by key — the human view of config and resolved
/// profile bodies.
fn print_fields(value: &Value) -> io::Result<()> {
    write!(io::stdout(), "{}", format_fields(value))
}

fn format_fields(value: &Value) -> String {
    fn walk(value: &Value, indent: usize, lines: &mut Vec<String>) {
        let Some(object) = value.as_object() else {
            lines.push(value.to_string());
            return;
        };
        let mut entries: Vec<(&String, &Value)> = object.iter().collect();
        entries.sort_by_key(|(key, _)| key.as_str());
        for (key, val) in entries {
            let pad = " ".repeat(indent);
            match val {
                Value::Object(_) => {
                    lines.push(format!("{pad}{key}:"));
                    walk(val, indent + 2, lines);
                }
                Value::Null => lines.push(format!("{pad}{key} =")),
                Value::String(s) => lines.push(format!("{pad}{key} = {s}")),
                other => lines.push(format!("{pad}{key} = {other}")),
            }
        }
    }

    let mut lines = Vec::new();
    walk(value, 0, &mut lines);
    lines.join("\n") + "\n"
}

/// Print a reply: pretty JSON with `--json`, aligned fields otherwise.
/// Going through `serde_json::Value` keeps key order sorted and stable.
pub(crate) fn print_reply(reply: &impl serde::Serialize, json_out: bool) -> Result<()> {
    let value = serde_json::to_value(reply)?;
    if json_out {
        writeln!(io::stdout(), "{}", serde_json::to_string_pretty(&value)?)?;
    } else {
        print_fields(&value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn format_fields_sorts_and_indents_nested_values() {
        let value = serde_json::json!({
            "zeta": 2,
            "alpha": {
                "text": "value",
                "empty": null,
                "enabled": true
            }
        });
        assert_eq!(
            super::format_fields(&value),
            "alpha:\n  empty =\n  enabled = true\n  text = value\nzeta = 2\n"
        );
    }

    #[test]
    fn format_fields_renders_non_object_values_as_json() {
        assert_eq!(super::format_fields(&serde_json::json!([1, 2])), "[1,2]\n");
    }
}
