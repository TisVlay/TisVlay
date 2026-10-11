//! Crash-safe file replacement for the services' state files.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Serialize;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};
use tracing::warn;

/// Replace `path` with `contents` atomically and durably: write a sibling
/// temp file (mode 0600, since these files hold credentials), fsync it,
/// rename it over `path`, then fsync the directory so the rename itself
/// survives a power loss. A crash at any point leaves either the old file
/// or the new one, never a torn or empty one. The temp file is removed if
/// any step fails.
///
/// Blocking: async callers run it on `spawn_blocking`.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let tmp = temp_path(path)?;
    // A temp file left by a crashed write may carry other permissions;
    // `create_new` below only applies the mode to a fresh file.
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Move `path` aside to `<path>.edited` (mode 0600, replacing an earlier
/// one) and return where it went: a state writer's way to keep a hand
/// edit it's about to overwrite. A single slot rather than timestamped
/// copies, since these files hold credentials and copies would pile up.
///
/// Blocking: async callers run it on `spawn_blocking`.
pub fn set_aside(path: &Path) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path names no file"))?
        .to_owned();
    name.push(".edited");
    let aside = path.with_file_name(name);
    fs::rename(path, &aside)?;
    fs::set_permissions(&aside, fs::Permissions::from_mode(0o600))?;
    Ok(aside)
}

/// What [`write_toml`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TomlWrite {
    /// The file now holds the new value.
    Written,
    /// The file already held the value, so it was left untouched.
    Unchanged,
}

/// Write `value` to the TOML file at `path` through [`write_atomic`],
/// keeping what a hand edit added: comments, spacing, key order and
/// quoting survive wherever the value they sit on is unchanged.
///
/// The new value is serialized as `toml::to_string_pretty` would, then
/// reconciled into the file's existing document: changed values are
/// replaced in place (keeping their trailing comment), keys that are gone
/// are removed, new ones are appended, and arrays of tables follow the new
/// value's order, each entry matched to the old one with the same `id`
/// (by position when entries carry no `id`), so a comment above an entry
/// moves with it. A missing or unparsable file is written fresh.
///
/// The reconciled text is parsed back and compared with the new value
/// before it's written. If they differ, which would be a reconcile bug,
/// the file is written fresh instead, with a warning: losing comments
/// beats storing the wrong state. A file that already holds the value is
/// not rewritten at all.
///
/// Blocking: async callers run it on `spawn_blocking`.
pub fn write_toml<T: Serialize>(path: &Path, value: &T) -> io::Result<TomlWrite> {
    let fresh = toml::to_string_pretty(value).map_err(io::Error::other)?;
    let existing = match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let text = match &existing {
        Some(old) => reconciled(path, old, &fresh).unwrap_or(fresh),
        None => fresh,
    };
    if existing.as_deref() == Some(text.as_str()) {
        return Ok(TomlWrite::Unchanged);
    }
    write_atomic(path, text.as_bytes())?;
    Ok(TomlWrite::Written)
}

/// `fresh` reconciled into the document `old`, or `None` when `old`
/// doesn't parse or the result wouldn't read back as `fresh`.
fn reconciled(path: &Path, old: &str, fresh: &str) -> Option<String> {
    let mut doc = old.parse::<DocumentMut>().ok()?;
    let new = fresh.parse::<DocumentMut>().ok()?;
    reconcile_table(doc.as_table_mut(), new.as_table());
    renumber(doc.as_table_mut(), &mut 0);
    let text = doc.to_string();
    let same =
        toml::from_str::<toml::Table>(&text).ok() == toml::from_str::<toml::Table>(fresh).ok();
    if !same {
        warn!(
            file = %path.display(),
            "keeping the file's comments would change what it says; writing it fresh"
        );
        return None;
    }
    Some(text)
}

fn reconcile_table(old: &mut Table, new: &Table) {
    old.retain(|key, _| new.contains_key(key));
    for (key, new_item) in new.iter() {
        match old.get_mut(key) {
            Some(old_item) => reconcile_item(old_item, new_item),
            None => {
                old.insert(key, new_item.clone());
            }
        }
    }
}

fn reconcile_item(old: &mut Item, new: &Item) {
    match (old, new) {
        (Item::Table(old), Item::Table(new)) => reconcile_table(old, new),
        (Item::ArrayOfTables(old), Item::ArrayOfTables(new)) => reconcile_entries(old, new),
        (Item::Value(old), Item::Value(new)) => {
            if !same_value(old, new) {
                let decor = old.decor().clone();
                *old = new.clone();
                *old.decor_mut() = decor;
            }
        }
        (old, new) => *old = new.clone(),
    }
}

/// The entries in `new`'s order, each reconciled into its old
/// counterpart (same `id`, else same position) so its comments follow it.
fn reconcile_entries(old: &mut ArrayOfTables, new: &ArrayOfTables) {
    fn id(table: &Table) -> Option<&str> {
        table.get("id")?.as_str()
    }
    let mut previous: Vec<Option<Table>> = old.iter().cloned().map(Some).collect();
    let mut entries = ArrayOfTables::new();
    for (index, new) in new.iter().enumerate() {
        let matched = match id(new) {
            Some(new_id) => previous
                .iter_mut()
                .find(|slot| slot.as_ref().and_then(id) == Some(new_id))
                .and_then(Option::take),
            None => previous.get_mut(index).and_then(Option::take),
        };
        entries.push(match matched {
            Some(mut table) => {
                reconcile_table(&mut table, new);
                table
            }
            None => new.clone(),
        });
    }
    *old = entries;
}

/// Equal values, however they're spelled (quoting, digit separators).
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Float(a), Value::Float(b)) => a.value().to_bits() == b.value().to_bits(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::Datetime(a), Value::Datetime(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_value(a, b))
        }
        (Value::InlineTable(a), Value::InlineTable(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same_value(a, b)))
        }
        _ => false,
    }
}

/// Give every table a document position in nesting order. toml_edit
/// writes tables sorted by position, and tables taken from two documents
/// carry positions from both: unrenumbered, a `[tv.config]` could land
/// under another entry's `[[tv]]` header and change which entry it
/// belongs to.
fn renumber(table: &mut Table, next: &mut isize) {
    for (_, item) in table.iter_mut() {
        let tables: Vec<&mut Table> = match item {
            Item::Table(table) => vec![table],
            Item::ArrayOfTables(entries) => entries.iter_mut().collect(),
            _ => continue,
        };
        for table in tables {
            *next = next.saturating_add(1);
            table.set_position(Some(*next));
            renumber(table, next);
        }
    }
}

/// `<path>.tmp`, beside the target so the rename stays on one filesystem.
fn temp_path(path: &Path) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path names no file"))?
        .to_owned();
    name.push(".tmp");
    Ok(path.with_file_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_owner_only_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/state.toml");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!dir.path().join("nested/state.toml.tmp").exists());
    }

    #[test]
    fn a_stale_temp_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        let stale = dir.path().join("state.toml.tmp");
        fs::write(&stale, b"stale").unwrap();
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic(&path, b"fresh").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"fresh");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn set_aside_moves_the_file_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        fs::write(&path, b"hand edit").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let aside = set_aside(&path).unwrap();
        assert_eq!(aside, dir.path().join("state.toml.edited"));
        assert!(!path.exists());
        assert_eq!(fs::read(&aside).unwrap(), b"hand edit");
        assert_eq!(
            fs::metadata(&aside).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[derive(Serialize)]
    struct Store {
        version: u32,
        tv: Vec<Tv>,
    }

    #[derive(Serialize, Clone)]
    struct Tv {
        id: String,
        name: String,
        config: std::collections::BTreeMap<String, String>,
    }

    fn tv(id: &str, name: &str, ip: &str) -> Tv {
        Tv {
            id: id.into(),
            name: name.into(),
            config: [("ip".to_string(), ip.to_string())].into(),
        }
    }

    fn store(tvs: &[Tv]) -> Store {
        Store {
            version: 1,
            tv: tvs.to_vec(),
        }
    }

    const HAND_EDITED: &str = r#"# Hand-tuned store.
version = 1

# The den set, by the window.
[[tv]]
id = "a"
name = "Den" # renamed on the TV too

[tv.config]
ip = "192.168.1.115"

[[tv]]
id = "b"
name = "Office"

[tv.config]
ip = "192.168.4.170"
"#;

    #[test]
    fn a_new_file_is_written_as_pretty_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.toml");
        let value = store(&[tv("a", "Den", "192.168.1.115")]);
        assert_eq!(write_toml(&path, &value).unwrap(), TomlWrite::Written);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            toml::to_string_pretty(&value).unwrap()
        );
    }

    #[test]
    fn an_unchanged_value_leaves_a_hand_edit_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.toml");
        fs::write(&path, HAND_EDITED).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let value = store(&[
            tv("a", "Den", "192.168.1.115"),
            tv("b", "Office", "192.168.4.170"),
        ]);
        assert_eq!(write_toml(&path, &value).unwrap(), TomlWrite::Unchanged);
        assert_eq!(fs::read_to_string(&path).unwrap(), HAND_EDITED);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "an unchanged file isn't rewritten");
    }

    #[test]
    fn a_change_keeps_the_comments_around_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.toml");
        fs::write(&path, HAND_EDITED).unwrap();
        let value = store(&[
            tv("a", "Den TV", "192.168.1.115"),
            tv("b", "Office", "192.168.4.99"),
        ]);
        assert_eq!(write_toml(&path, &value).unwrap(), TomlWrite::Written);
        let expected = HAND_EDITED
            .replace(r#"name = "Den" #"#, r#"name = "Den TV" #"#)
            .replace("192.168.4.170", "192.168.4.99");
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn entries_follow_the_new_order_and_take_their_comments_along() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.toml");
        fs::write(&path, HAND_EDITED).unwrap();
        // Office first, Den gone, a new entry appended.
        let value = store(&[
            tv("b", "Office", "192.168.4.170"),
            tv("c", "Bedroom", "10.0.0.2"),
        ]);
        write_toml(&path, &value).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("den set"),
            "a removed entry takes its comment:\n{text}"
        );
        assert!(text.starts_with("# Hand-tuned store."), "{text}");
        let back: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(back, toml::Table::try_from(&value).unwrap());
    }

    #[test]
    fn a_reordered_entry_keeps_its_own_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.toml");
        fs::write(&path, HAND_EDITED).unwrap();
        let value = store(&[
            tv("b", "Office", "192.168.4.170"),
            tv("a", "Den", "192.168.1.115"),
        ]);
        write_toml(&path, &value).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let back: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(back, toml::Table::try_from(&value).unwrap(), "{text}");
        let den = text.find("# The den set").unwrap();
        assert!(
            den > text.find(r#"id = "b""#).unwrap(),
            "the comment moved with its entry:\n{text}"
        );
    }

    #[test]
    fn an_unparsable_file_is_written_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.toml");
        fs::write(&path, "version = = 1").unwrap();
        let value = store(&[tv("a", "Den", "192.168.1.115")]);
        assert_eq!(write_toml(&path, &value).unwrap(), TomlWrite::Written);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            toml::to_string_pretty(&value).unwrap()
        );
    }

    #[test]
    fn a_failed_write_removes_its_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        // Renaming a file over a directory fails after the temp is written.
        let path = dir.path().join("occupied");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"x").unwrap();
        assert!(write_atomic(&path, b"data").is_err());
        assert!(!dir.path().join("occupied.tmp").exists());
    }
}
