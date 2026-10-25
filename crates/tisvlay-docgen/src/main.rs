//! Renders the mdBook user guide's generated partials from the code they
//! document, so the book can't drift from it:
//!
//! - `cli.md` — the `tisvlay` command tree (the CLI reference).
//! - `profile-defaults-system.md` / `profile-defaults-user.md` — the stock
//!   profile mappings, each field under its doc comment.
//! - `release-version.txt` — the release the install commands download.
//!
//! Run `gen-docs <output dir>`; `just docs-gen` writes the partials into
//! `docs/book/src/generated/` and `just docs-gen-check` fails when the
//! committed copies are stale. Every partial is emitted hook-clean (no
//! trailing whitespace, one final newline) so a regeneration and the
//! committed file always agree.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tisvlay_core::{
    SYSTEM_PROFILE_FIELDS, SystemProfileConfig, USER_PROFILE_FIELDS, UserProfileConfig,
};

/// The profile structs' source, scanned for each field's doc comment.
const CONFIG_SOURCE: &str = include_str!("../../tisvlay-core/src/config.rs");

fn main() -> Result<()> {
    let out = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: gen-docs <output dir>")?;
    fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;

    write(&out, "cli.md", &cli_reference())?;
    write(
        &out,
        "profile-defaults-system.md",
        &profile_defaults(
            "SystemProfileConfig",
            &SystemProfileConfig::full_default(),
            SYSTEM_PROFILE_FIELDS,
        )?,
    )?;
    write(
        &out,
        "profile-defaults-user.md",
        &profile_defaults(
            "UserProfileConfig",
            &UserProfileConfig::full_default(),
            USER_PROFILE_FIELDS,
        )?,
    )?;
    write(
        &out,
        "release-version.txt",
        &format!("export VERSION={}\n", env!("CARGO_PKG_VERSION")),
    )?;
    Ok(())
}

fn write(dir: &Path, name: &str, contents: &str) -> Result<()> {
    let path = dir.join(name);
    fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))
}

/// The clap tree as Markdown, starting at "Command Overview": the CLI
/// page supplies its own `# CLI Reference` heading and intro, while
/// clap-markdown always emits an H1 title and a boilerplate preamble, so
/// that leading run is dropped (and the tool's footer turned off).
fn cli_reference() -> String {
    let markdown = clap_markdown::help_markdown_custom::<tisvlay_cli::Cli>(
        &clap_markdown::MarkdownOptions::new().show_footer(false),
    );
    // Per-line trim: clap-markdown leaves a trailing space after the em
    // dash of an undocumented subcommand, which the whitespace hook would
    // strip right back.
    let body = markdown
        .lines()
        .skip_while(|line| {
            line.trim().is_empty()
                || line.starts_with("# ")
                || line.starts_with("This document contains the help content")
        })
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}\n", body.trim_end())
}

/// A fenced TOML block of a profile kind's stock mapping, in `fields`
/// order, each field under its doc comment. The rendered TOML is parsed
/// back and must equal `defaults`, so a rendering shortcut (see
/// [`render_value`]) can never change what a reader would copy.
fn profile_defaults<T>(struct_name: &str, defaults: &T, fields: &[&str]) -> Result<String>
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let toml::Value::Table(table) = toml::Value::try_from(defaults)? else {
        bail!("{struct_name} didn't serialize to a table");
    };
    let docs = field_docs(struct_name)?;
    let mut entries = Vec::with_capacity(fields.len());
    for &field in fields {
        let value = table
            .get(field)
            .with_context(|| format!("{struct_name}.{field} is missing from its full default"))?;
        let doc = docs
            .get(field)
            .filter(|doc| !doc.is_empty())
            .with_context(|| format!("{struct_name}.{field} has no doc comment"))?;
        let mut entry: Vec<String> = doc
            .iter()
            .map(|line| {
                if line.is_empty() {
                    "#".to_string()
                } else {
                    format!("# {line}")
                }
            })
            .collect();
        entry.push(format!("{field} = {}", render_value(value)));
        entries.push(entry.join("\n"));
    }
    let body = entries.join("\n\n");

    // `name` is the one non-inheritable field; full_default leaves it empty.
    let reparsed: T = toml::from_str(&format!("name = \"\"\n{body}"))
        .with_context(|| format!("the rendered {struct_name} defaults don't parse"))?;
    if reparsed != *defaults {
        bail!("the rendered {struct_name} defaults parse to {reparsed:?}, not {defaults:?}");
    }
    Ok(format!("```toml\n{body}\n```\n"))
}

/// One field's value as a reader would write it: an action table drops
/// its `false` modifiers (their serde default), and collapses to the bare
/// action token when no modifier is left.
fn render_value(value: &toml::Value) -> String {
    let toml::Value::Table(table) = value else {
        return value.to_string();
    };
    let kept: Vec<(&String, &toml::Value)> = table
        .iter()
        .filter(|(_, value)| value.as_bool() != Some(false))
        .collect();
    match kept.as_slice() {
        [(key, action)] if key.as_str() == "action" => action.to_string(),
        _ => {
            let pairs: Vec<String> = kept
                .iter()
                .map(|(key, value)| format!("{key} = {value}"))
                .collect();
            format!("{{ {} }}", pairs.join(", "))
        }
    }
}

/// Each named field's `///` doc lines in `pub struct <struct_name>`, from
/// the rustfmt-normalized config source: doc lines accumulate until the
/// `pub <field>:` they annotate (attributes in between are skipped), and a
/// blank line drops any unclaimed run.
fn field_docs(struct_name: &str) -> Result<BTreeMap<String, Vec<String>>> {
    let header = format!("pub struct {struct_name} {{");
    let mut lines = CONFIG_SOURCE
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != header);
    lines
        .next()
        .with_context(|| format!("`{header}` not found in tisvlay-core's config.rs"))?;

    let mut docs = BTreeMap::new();
    let mut pending: Vec<String> = Vec::new();
    for line in lines {
        if line == "}" {
            break;
        }
        if let Some(doc) = line.strip_prefix("///") {
            pending.push(doc.strip_prefix(' ').unwrap_or(doc).to_string());
        } else if let Some((field, _)) = line
            .strip_prefix("pub ")
            .and_then(|rest| rest.split_once(':'))
        {
            docs.insert(field.to_string(), std::mem::take(&mut pending));
        } else if line.is_empty() {
            pending.clear();
        }
    }
    Ok(docs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_field_has_a_doc_comment() {
        for (struct_name, fields) in [
            ("SystemProfileConfig", SYSTEM_PROFILE_FIELDS),
            ("UserProfileConfig", USER_PROFILE_FIELDS),
        ] {
            let docs = field_docs(struct_name).unwrap();
            for field in fields {
                assert!(
                    docs.get(*field).is_some_and(|doc| !doc.is_empty()),
                    "{struct_name}.{field} has no doc comment"
                );
            }
        }
    }

    #[test]
    fn profile_defaults_round_trip() {
        profile_defaults(
            "SystemProfileConfig",
            &SystemProfileConfig::full_default(),
            SYSTEM_PROFILE_FIELDS,
        )
        .unwrap();
        profile_defaults(
            "UserProfileConfig",
            &UserProfileConfig::full_default(),
            USER_PROFILE_FIELDS,
        )
        .unwrap();
    }

    #[test]
    fn action_tables_render_only_their_set_modifiers() {
        let bare: toml::Value = toml::from_str::<toml::Table>(
            "v = { action = \"screen_on\", restore_input = false, while_streaming = false }",
        )
        .unwrap()["v"]
            .clone();
        assert_eq!(render_value(&bare), "\"screen_on\"");

        let modified: toml::Value = toml::from_str::<toml::Table>(
            "v = { action = \"power_on\", restore_input = true, while_streaming = false }",
        )
        .unwrap()["v"]
            .clone();
        assert_eq!(
            render_value(&modified),
            "{ action = \"power_on\", restore_input = true }"
        );
    }
}
