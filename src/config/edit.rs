//! The shared TOML edit lifecycle: read -> parse -> mutate -> write ->
//! reload+validate, with automatic rollback if the post-write config
//! would be invalid.
//!
//! Every config-mutating command (`add-agent`, `add-channel`,
//! `add-host`, `set-swarm-boss`) plus the in-place TOML edits in
//! `teleport` and `takeover` route their canonical-TOML writes through
//! [`edit_then_validate_with_rollback`]. This centralizes the "never
//! leave a half-committed config on disk" invariant in one place
//! instead of every mutator hand-rolling its own read/parse/write/
//! reload/rollback dance.
//!
//! The generic `toml_edit` helpers (`ensure_array_of_tables`,
//! `append_channel`) used by the mutators also live here so they can be
//! shared without one mutator reaching into another's module.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

use crate::config::resolve::DerivedChannel;
use crate::config::Config;

/// Atomically mutate the canonical TOML at `path`: read -> parse ->
/// apply `mutate` -> write -> reload+validate. If `mutate` errors, the
/// file is never written. If the post-write reload/validate fails, the
/// ORIGINAL contents are restored (no half-committed config) and the
/// validation error is returned. On success returns the reloaded Config.
///
/// `Config::load` validates internally (see `config/load.rs`), so a
/// successful reload here is also a successful validation.
pub fn edit_then_validate_with_rollback(
    path: &Path,
    mutate: impl FnOnce(&mut DocumentMut) -> Result<()>,
) -> Result<Config> {
    let original =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc: DocumentMut = original
        .parse()
        .with_context(|| format!("parsing {} as TOML", path.display()))?;
    mutate(&mut doc)?;
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))?;
    match Config::load(path) {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            // rollback — best-effort restore of the original bytes
            let _ = std::fs::write(path, &original);
            Err(e).with_context(|| {
                format!(
                    "{} would be invalid after the edit — rolled back, no change written",
                    path.display()
                )
            })
        }
    }
}

// --------------------------------------------------------------- toml_edit helpers

/// Ensure `doc[key]` is an array-of-tables, creating an empty one when
/// the key is absent. Errors if the key exists but holds a non-AoT
/// value (a malformed config we shouldn't silently clobber).
pub(crate) fn ensure_array_of_tables<'a>(
    doc: &'a mut DocumentMut,
    key: &str,
) -> Result<&'a mut ArrayOfTables> {
    if !doc.contains_key(key) {
        doc.insert(key, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    doc.get_mut(key)
        .and_then(|i| i.as_array_of_tables_mut())
        .ok_or_else(|| anyhow!("config key `{}` exists but is not an array of tables", key))
}

/// Append a `[[channels]]` block built from a derived channel. Shared
/// by `add-agent` (one block per peer) and `add-channel` (the single
/// new bilateral).
pub(crate) fn append_channel(doc: &mut DocumentMut, ch: &DerivedChannel) -> Result<()> {
    let channels = ensure_array_of_tables(doc, "channels")?;
    let mut block = Table::new();
    block["file"] = value(ch.file.as_str());
    block["side"] = value(ch.side.as_str());
    let mut participants = Array::new();
    participants.push(ch.participants[0].as_str());
    participants.push(ch.participants[1].as_str());
    block["participants"] = value(participants);
    block["purpose"] = value(ch.purpose.as_str());
    channels.push(block);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn minimal_two_agent() -> &'static str {
        r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"
"#
    }

    /// (a) A successful edit returns the reloaded Config AND the file on
    /// disk reflects the mutation.
    #[test]
    fn successful_edit_returns_config_and_persists() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("giga-harness.toml");
        fs::write(&path, minimal_two_agent()).unwrap();

        let cfg = edit_then_validate_with_rollback(&path, |doc| {
            append_channel(
                doc,
                &DerivedChannel {
                    file: "alice-bob.md".into(),
                    side: "wsl".into(),
                    participants: ["alice".into(), "bob".into()],
                    purpose: "test".into(),
                },
            )
        })
        .unwrap();

        // Returned config sees the new channel.
        assert!(cfg.channels.iter().any(|c| c.file == "alice-bob.md"));
        // File on disk reflects it too.
        let on_disk = fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains(r#"file = "alice-bob.md""#));
    }

    /// (b) A mutate closure that produces an INVALID config (a channel
    /// referencing a nonexistent participant) leaves the file
    /// byte-identical to the original and returns Err.
    #[test]
    fn invalid_edit_rolls_back_and_returns_err() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("giga-harness.toml");
        fs::write(&path, minimal_two_agent()).unwrap();
        let before = fs::read_to_string(&path).unwrap();

        let err = edit_then_validate_with_rollback(&path, |doc| {
            // Reference a participant that doesn't exist → validation
            // fails on reload → rollback.
            append_channel(
                doc,
                &DerivedChannel {
                    file: "alice-ghost.md".into(),
                    side: "wsl".into(),
                    participants: ["alice".into(), "ghost".into()],
                    purpose: "bad".into(),
                },
            )
        })
        .unwrap_err();

        // Error explains the rollback.
        assert!(
            err.to_string().contains("rolled back"),
            "error should mention rollback, got: {err:#}",
        );
        // File is byte-identical to the original.
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "rollback must restore the exact bytes");
    }

    /// A mutate closure that itself errors never writes the file.
    #[test]
    fn mutate_error_never_writes() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("giga-harness.toml");
        fs::write(&path, minimal_two_agent()).unwrap();
        let before = fs::read_to_string(&path).unwrap();

        let err = edit_then_validate_with_rollback(&path, |_doc| Err(anyhow!("mutate refused")))
            .unwrap_err();
        assert!(err.to_string().contains("mutate refused"));
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "a failing mutate must not write the file");
    }
}
