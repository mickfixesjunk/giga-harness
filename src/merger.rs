//! `giga merger` — append per-host slice events into the watched merged file.
//!
//! Per REMOTE_DESIGN.md §2.2: every cross-host channel has per-host
//! single-writer slice files `<channel>.<host>.md` next to the merged
//! `<channel>.md`. The merger is the SOLE writer to the merged file —
//! it polls all slice files (own + peers') and appends new bytes to
//! `<channel>.md` in receive-order. The watcher tails the merged file
//! exactly as today; remote messages just appear there as ordinary
//! appends.
//!
//! Why merger-as-sole-writer (rather than post double-writing): the
//! merged file has one append-only writer, so the watcher's
//! `len() > last_size` invariant is never violated by concurrent
//! writes. Post writes only to the slice; the merger picks it up and
//! the watcher sees it within one tick.
//!
//! Cursors: `~/.giga/merge-cursors/<channel>/<host>.pos` — single
//! ASCII u64 of bytes-consumed-from-that-slice. Same format as watch
//! cursors but in a separate namespace. Persisted AFTER successful
//! append (crash mid-flush re-delivers, never loses).
//!
//! Local-only channels (all participants on `this_host`) are skipped —
//! post wrote directly to the merged file via the fast-path. The
//! merger only touches files for cross-host channels.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::cursor;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const RELOAD_EVERY_N_TICKS: u64 = 5;

/// Per-channel merge state — one merged file + N slice files (one per
/// host that has at least one participant on this channel).
struct ChannelMergeState {
    /// Channel filename, e.g. "design-code-2.md". Used as the
    /// merge-cursor namespace key.
    name: String,
    /// Absolute path to `<channel>.md`, the merged file the watcher
    /// tails. The merger is the sole writer to this file.
    merged_path: PathBuf,
    /// Slice files keyed by their owning host name. Each slice is
    /// single-writer (only that host's `giga post` appends to it).
    slices: HashMap<String, SliceState>,
}

struct SliceState {
    path: PathBuf,
    last_size: u64,
}

pub fn run(config_path: &Path, once: bool) -> Result<()> {
    if !config_path.exists() {
        anyhow::bail!(
            "config file not found: {} — pass --config <path>",
            config_path.display(),
        );
    }
    let giga_home = cursor::giga_home();
    let mut tracked: HashMap<String, ChannelMergeState> = HashMap::new();
    let mut tick: u64 = 0;

    refresh_tracked(config_path, &mut tracked, giga_home.as_deref());
    if tracked.is_empty() {
        eprintln!(
            "merger: no cross-host channels in {} — sitting idle, will reload config every ~{}s",
            config_path.display(),
            POLL_INTERVAL.as_secs() * RELOAD_EVERY_N_TICKS,
        );
    } else {
        eprintln!(
            "merger: tracking {} cross-host channel(s): {}",
            tracked.len(),
            tracked.keys().cloned().collect::<Vec<_>>().join(", "),
        );
    }

    if once {
        // One sweep — useful for tests + scripted catch-up scenarios.
        merge_tick(&mut tracked, giga_home.as_deref());
        return Ok(());
    }

    loop {
        thread::sleep(POLL_INTERVAL);
        tick = tick.wrapping_add(1);
        if tick % RELOAD_EVERY_N_TICKS == 0 {
            refresh_tracked(config_path, &mut tracked, giga_home.as_deref());
        }
        merge_tick(&mut tracked, giga_home.as_deref());
    }
}

/// One merge sweep across all tracked channels + slices. Pure-ish (the
/// side effects are deterministic file I/O); extracted so tests can
/// invoke it without the 3s sleep loop.
fn merge_tick(
    tracked: &mut HashMap<String, ChannelMergeState>,
    giga_home: Option<&Path>,
) {
    for (channel_name, state) in tracked.iter_mut() {
        for (slice_host, slice) in state.slices.iter_mut() {
            let cur = match fs::metadata(&slice.path) {
                Ok(m) => m.len(),
                Err(_) => continue, // slice doesn't exist yet — fine, will appear later
            };
            if cur < slice.last_size {
                // Slice was truncated or replaced — reset so we re-read
                // from the new start. This shouldn't happen in normal
                // operation (slices are append-only) but defensive.
                slice.last_size = cur;
                continue;
            }
            if cur == slice.last_size {
                continue;
            }
            // Read the new bytes from the slice...
            let delta = match read_delta(&slice.path, slice.last_size, cur) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!(
                        "merger: failed reading delta from {}: {e}",
                        slice.path.display()
                    );
                    continue;
                }
            };
            // ...and append them verbatim to the merged file. Open with
            // append+create so first writes work; the merged file may not
            // exist yet on a brand-new channel.
            if let Err(e) = append_bytes(&state.merged_path, &delta) {
                eprintln!(
                    "merger: failed appending to {}: {e}",
                    state.merged_path.display()
                );
                continue;
            }
            // Persist the cursor ONLY after the append succeeded — a
            // crash mid-write re-delivers next tick, never loses.
            slice.last_size = cur;
            if let Some(home) = giga_home {
                cursor::write_merge(home, channel_name, slice_host, cur);
            }
        }
    }
}

/// Re-derive the tracked set from the config: for every cross-host
/// channel, enumerate one slice per host that has a participant on
/// the channel. Adds new channels/slices; drops channels removed from
/// the config.
fn refresh_tracked(
    config_path: &Path,
    tracked: &mut HashMap<String, ChannelMergeState>,
    giga_home: Option<&Path>,
) {
    let cfg = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("merger: config reload failed ({e}) — keeping current tracked set");
            return;
        }
    };

    let active = compute_active_channels(&cfg);
    let active_names: HashSet<&str> = active.iter().map(|(n, _, _)| n.as_str()).collect();

    for (name, merged_path, slice_hosts) in &active {
        // Build/refresh per-channel state.
        let entry = tracked.entry(name.clone()).or_insert_with(|| ChannelMergeState {
            name: name.clone(),
            merged_path: merged_path.clone(),
            slices: HashMap::new(),
        });
        // Drop slices for hosts no longer participating.
        entry.slices.retain(|h, _| slice_hosts.iter().any(|sh| sh == h));
        // Add slices we don't have yet.
        for host in slice_hosts {
            if entry.slices.contains_key(host) {
                continue;
            }
            let slice_path = derive_slice_path(merged_path, host);
            let start = giga_home
                .and_then(|home| cursor::read_merge(home, name, host))
                .unwrap_or(0);
            entry.slices.insert(
                host.clone(),
                SliceState {
                    path: slice_path,
                    last_size: start,
                },
            );
        }
    }

    // Drop channels that are no longer cross-host (or were removed).
    let to_drop: Vec<String> = tracked
        .keys()
        .filter(|k| !active_names.contains(k.as_str()))
        .cloned()
        .collect();
    for name in to_drop {
        tracked.remove(&name);
        eprintln!("merger: dropped channel `{name}` (no longer cross-host or removed)");
    }
}

/// For every cross-host channel in the config, return:
///   (channel_filename, absolute_merged_path, sorted_distinct_slice_hosts)
fn compute_active_channels(cfg: &Config) -> Vec<(String, PathBuf, Vec<String>)> {
    cfg.channels
        .iter()
        .filter(|ch| !cfg.channel_is_local(ch)) // skip fast-path-local channels
        .filter_map(|ch| {
            let merged_path = cfg.channel_path(ch).ok()?;
            // Find every distinct host with at least one participant on
            // this channel — those are the slice files we'll watch.
            let mut hosts: Vec<String> = ch
                .participants
                .iter()
                .filter_map(|p| {
                    cfg.agents
                        .iter()
                        .find(|a| a.name == *p)
                        .and_then(|a| cfg.agent_host(a))
                        .map(|s| s.to_string())
                })
                .collect();
            hosts.sort();
            hosts.dedup();
            Some((ch.file.clone(), merged_path, hosts))
        })
        .collect()
}

/// Given `/dir/<channel>.md` + a host, derive `/dir/<channel>.<host>.md`.
/// Mirrors `post::slice_path`.
fn derive_slice_path(merged: &Path, host: &str) -> PathBuf {
    let parent = merged.parent().unwrap_or_else(|| Path::new("."));
    let stem = merged
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "channel".to_string());
    parent.join(format!("{stem}.{host}.md"))
}

fn read_delta(path: &Path, from: u64, to: u64) -> Result<Vec<u8>> {
    let mut f = fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0u8; (to - from) as usize];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

fn append_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .with_context(|| format!("opening {} for append", path.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("writing to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn derive_slice_path_basic() {
        let merged = Path::new("/inbox/alice-bob.md");
        assert_eq!(
            derive_slice_path(merged, "wsl-a"),
            PathBuf::from("/inbox/alice-bob.wsl-a.md")
        );
    }

    #[test]
    fn derive_slice_path_handles_dotted_channel_name() {
        let merged = Path::new("/inbox/foo.bar.md");
        assert_eq!(
            derive_slice_path(merged, "h1"),
            PathBuf::from("/inbox/foo.bar.h1.md")
        );
    }

    /// Build a 2-host cross-host swarm fixture: 2 agents (alice on A,
    /// bob on B), 1 bilateral channel. Returns (tmp, config_path).
    fn cross_host_fixture(this_host: &str) -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let config_path = tmp.path().join("giga-harness.toml");
        let toml = format!(
            r#"
[project]
name = "remote-test"

[paths]
wsl_inbox = '{inbox}'

[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "wsl-a"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"
host = "wsl-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox = inbox.to_string_lossy(),
        );
        fs::write(&config_path, toml).unwrap();
        fs::write(
            tmp.path().join("this_host.toml"),
            format!("this_host = \"{this_host}\"\n"),
        )
        .unwrap();
        (tmp, config_path, inbox)
    }

    #[test]
    fn compute_active_channels_finds_cross_host() {
        let (_tmp, config_path, _inbox) = cross_host_fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        let active = compute_active_channels(&cfg);
        assert_eq!(active.len(), 1);
        let (name, _, hosts) = &active[0];
        assert_eq!(name, "alice-bob.md");
        assert_eq!(hosts, &vec!["wsl-a".to_string(), "wsl-b".to_string()]);
    }

    #[test]
    fn compute_active_channels_skips_local_only() {
        // Same fixture but with only one host -> channel is fully local
        // -> merger has nothing to do.
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let config_path = tmp.path().join("giga-harness.toml");
        let toml = format!(
            r#"
[project]
name = "x"

[paths]
wsl_inbox = '{inbox}'

[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "wsl-a"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"
host = "wsl-a"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox = inbox.to_string_lossy(),
        );
        fs::write(&config_path, toml).unwrap();
        fs::write(tmp.path().join("this_host.toml"), "this_host = \"wsl-a\"\n").unwrap();
        let cfg = Config::load(&config_path).unwrap();
        assert_eq!(compute_active_channels(&cfg).len(), 0);
    }

    #[test]
    fn merge_tick_appends_slice_growth_to_merged() {
        let (tmp, config_path, inbox) = cross_host_fixture("wsl-a");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));
        assert_eq!(tracked.len(), 1);

        // Write some content to the wsl-a slice (as if alice posted).
        let slice_a = inbox.join("alice-bob.wsl-a.md");
        fs::write(&slice_a, b"\n\n===\n[alice] hi - 2026-01-01T00:00:00Z\n===\n").unwrap();

        merge_tick(&mut tracked, Some(tmp.path()));

        let merged = inbox.join("alice-bob.md");
        let body = fs::read_to_string(&merged).unwrap();
        assert!(body.contains("[alice] hi"));

        // Cursor should be advanced to the slice's current length.
        let cursor = cursor::read_merge(tmp.path(), "alice-bob.md", "wsl-a");
        assert_eq!(cursor, Some(fs::metadata(&slice_a).unwrap().len()));
    }

    #[test]
    fn merge_tick_handles_multiple_slices_in_one_pass() {
        let (tmp, config_path, inbox) = cross_host_fixture("wsl-a");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));

        let slice_a = inbox.join("alice-bob.wsl-a.md");
        let slice_b = inbox.join("alice-bob.wsl-b.md");
        fs::write(&slice_a, b"\n\n===\n[alice] from-a - 2026-01-01T00:00:00Z\n===\n").unwrap();
        fs::write(&slice_b, b"\n\n===\n[bob] from-b - 2026-01-01T00:00:01Z\n===\n").unwrap();

        merge_tick(&mut tracked, Some(tmp.path()));

        let merged = fs::read_to_string(inbox.join("alice-bob.md")).unwrap();
        assert!(merged.contains("[alice] from-a"));
        assert!(merged.contains("[bob] from-b"));
    }

    #[test]
    fn merge_tick_is_idempotent_when_no_growth() {
        let (tmp, config_path, inbox) = cross_host_fixture("wsl-a");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));

        let slice_a = inbox.join("alice-bob.wsl-a.md");
        fs::write(&slice_a, b"\n\n===\n[alice] once - 2026-01-01T00:00:00Z\n===\n").unwrap();

        merge_tick(&mut tracked, Some(tmp.path()));
        merge_tick(&mut tracked, Some(tmp.path())); // no slice growth -> no-op
        merge_tick(&mut tracked, Some(tmp.path()));

        let merged = fs::read_to_string(inbox.join("alice-bob.md")).unwrap();
        // "once" should appear exactly once; no re-delivery.
        assert_eq!(merged.matches("[alice] once").count(), 1);
    }

    #[test]
    fn merge_tick_appends_incremental_growth() {
        let (tmp, config_path, inbox) = cross_host_fixture("wsl-a");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));

        let slice_a = inbox.join("alice-bob.wsl-a.md");

        // First post.
        fs::write(&slice_a, b"\n\n===\n[alice] one - 2026-01-01T00:00:00Z\n===\n").unwrap();
        merge_tick(&mut tracked, Some(tmp.path()));

        // Second post (append).
        fs::OpenOptions::new()
            .append(true)
            .open(&slice_a)
            .unwrap()
            .write_all(b"\n\n===\n[alice] two - 2026-01-01T00:00:01Z\n===\n")
            .unwrap();
        merge_tick(&mut tracked, Some(tmp.path()));

        let merged = fs::read_to_string(inbox.join("alice-bob.md")).unwrap();
        assert!(merged.contains("[alice] one"));
        assert!(merged.contains("[alice] two"));
        assert_eq!(merged.matches("[alice] one").count(), 1, "no re-delivery on incremental tick");
    }

    #[test]
    fn merge_tick_recovers_from_truncated_slice() {
        // Pathological: someone manually truncated a slice file. Merger
        // should reset its cursor and not panic; subsequent appends to
        // the slice get delivered fresh.
        let (tmp, config_path, inbox) = cross_host_fixture("wsl-a");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));

        let slice_a = inbox.join("alice-bob.wsl-a.md");
        fs::write(&slice_a, b"\n\n===\n[alice] before - 2026-01-01T00:00:00Z\n===\n").unwrap();
        merge_tick(&mut tracked, Some(tmp.path()));

        // Truncate.
        fs::write(&slice_a, b"").unwrap();
        merge_tick(&mut tracked, Some(tmp.path())); // cursor reset, no append

        // Re-write fresh.
        fs::write(&slice_a, b"\n\n===\n[alice] after - 2026-01-01T00:00:02Z\n===\n").unwrap();
        merge_tick(&mut tracked, Some(tmp.path()));

        let merged = fs::read_to_string(inbox.join("alice-bob.md")).unwrap();
        assert!(merged.contains("[alice] before"));
        assert!(merged.contains("[alice] after"));
    }

    #[test]
    fn refresh_tracked_drops_channels_that_become_local() {
        // Start cross-host, then rewrite config so bob also lives on
        // wsl-a -> channel becomes local-only -> merger drops it.
        let (tmp, config_path, _inbox) = cross_host_fixture("wsl-a");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));
        assert_eq!(tracked.len(), 1);

        // Rewrite config so bob is now on wsl-a too.
        let new_toml = fs::read_to_string(&config_path)
            .unwrap()
            .replace(r#"host = "wsl-b""#, r#"host = "wsl-a""#);
        fs::write(&config_path, new_toml).unwrap();

        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));
        assert_eq!(tracked.len(), 0, "channel should be dropped from tracked set");
    }
}
