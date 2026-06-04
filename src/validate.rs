//! `giga validate` — config sanity check, no side effects.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::Config;
use crate::fs_paths::to_host_fs;

pub fn run(path: &Path) -> Result<()> {
    let cfg = Config::load(path)?;
    println!(
        "ok: {} ({}) — {} agents, {} channels",
        path.display(),
        cfg.project.name,
        cfg.agents.len(),
        cfg.channels.len(),
    );
    if let Some(bp) = &cfg.bench_protocol {
        println!(
            "    bench scheduler: {} (slot pool: {})",
            bp.scheduler, bp.slot_pool
        );
    }
    for ch in &cfg.channels {
        let p = cfg.channel_path(ch)?;
        let status = if p.exists() {
            "exists"
        } else {
            "absent — `giga init` will create it"
        };
        println!("    [{}] {} ({})", ch.side, p.display(), status);
    }

    // Orphan detection: scan each configured inbox dir for files that
    // look like giga channel files (giga's standard `# X ↔ Y shared
    // inbox` header in line 1) but aren't enrolled in [[channels]].
    //
    // Caught us out at least once: an agent started a bilateral by
    // creating the inbox file directly, both sides armed per-channel
    // Monitors against it, and the channel worked invisibly for weeks
    // until the auto-discovery watcher migration silently stopped
    // reading it.
    let enrolled: HashSet<&str> = cfg.channels.iter().map(|c| c.file.as_str()).collect();
    let mut orphans: Vec<(&str, PathBuf, u64)> = Vec::new();
    for (side, dir_opt) in [
        ("wsl", cfg.paths.wsl_inbox.as_ref()),
        ("windows", cfg.paths.windows_inbox.as_ref()),
    ] {
        let Some(dir) = dir_opt else { continue };
        let host_dir = to_host_fs(dir);
        let Ok(entries) = fs::read_dir(&host_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if enrolled.contains(name) {
                continue;
            }
            if !looks_like_channel(&path) {
                continue;
            }
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            orphans.push((side, path, size));
        }
    }
    if !orphans.is_empty() {
        println!();
        println!(
            "warning: {} orphan channel file(s) on disk but not enrolled in [[channels]]:",
            orphans.len(),
        );
        for (side, p, size) in &orphans {
            println!("    [{}] {} ({} bytes)", side, p.display(), size);
        }
        println!(
            "    (orphans work for legacy per-channel watchers but are invisible to the auto-discovery watcher.\n     \
             Add a [[channels]] entry naming the file, or move/rename to archive.)",
        );
    }

    Ok(())
}

/// Cheap heuristic: a giga channel file's first line matches
/// `# <something> shared inbox`. Lets validate flag genuine orphan
/// channels without false-positiving on agent workdir files
/// (AGENTS.md, HANDOVER.md) that happen to share an inbox dir.
fn looks_like_channel(path: &Path) -> bool {
    let Ok(f) = fs::File::open(path) else {
        return false;
    };
    let mut buf = String::new();
    if BufReader::new(f).read_line(&mut buf).is_err() {
        return false;
    }
    let first = buf.trim_end();
    first.starts_with("# ") && first.contains("shared inbox")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn looks_like_channel_accepts_bilateral_header() {
        let tmp = TempDir::new().unwrap();
        let p = write_file(tmp.path(), "ch.md", "# alice ↔ bob shared inbox\n\nbody\n");
        assert!(looks_like_channel(&p));
    }

    #[test]
    fn looks_like_channel_accepts_single_party_header() {
        let tmp = TempDir::new().unwrap();
        // Some channels (e.g. broadcasts) have one-party-style headers.
        let p = write_file(tmp.path(), "ch.md", "# everyone shared inbox\n");
        assert!(looks_like_channel(&p));
    }

    #[test]
    fn looks_like_channel_rejects_agents_md() {
        let tmp = TempDir::new().unwrap();
        let p = write_file(tmp.path(), "AGENTS.md", "# alice agent\n\nYou are...\n");
        assert!(!looks_like_channel(&p));
    }

    #[test]
    fn looks_like_channel_rejects_handover() {
        let tmp = TempDir::new().unwrap();
        let p = write_file(tmp.path(), "HANDOVER.md", "# Handover notes\n\n...\n");
        assert!(!looks_like_channel(&p));
    }

    #[test]
    fn looks_like_channel_rejects_random_md() {
        let tmp = TempDir::new().unwrap();
        let p = write_file(tmp.path(), "scratch.md", "random notes\nno header\n");
        assert!(!looks_like_channel(&p));
    }

    #[test]
    fn looks_like_channel_rejects_missing_file() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("does-not-exist.md");
        assert!(!looks_like_channel(&p));
    }

    #[test]
    fn looks_like_channel_rejects_empty_file() {
        let tmp = TempDir::new().unwrap();
        let p = write_file(tmp.path(), "empty.md", "");
        assert!(!looks_like_channel(&p));
    }

    #[test]
    fn looks_like_channel_rejects_header_without_shared_inbox() {
        let tmp = TempDir::new().unwrap();
        let p = write_file(tmp.path(), "x.md", "# Some Document Title\n");
        assert!(!looks_like_channel(&p));
    }

    #[test]
    fn looks_like_channel_accepts_handoff_txt_style() {
        // handoff.txt (legacy channel naming) uses the same header.
        let tmp = TempDir::new().unwrap();
        let p = write_file(
            tmp.path(),
            "handoff.txt",
            "# alice ↔ bob shared inbox\n",
        );
        assert!(looks_like_channel(&p));
    }
}
