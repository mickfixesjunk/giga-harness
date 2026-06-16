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
use std::path::{Path, PathBuf};
use std::thread;

use anyhow::Result;

use crate::config::Config;
use crate::coordination::cursor;
use crate::foundation::append::append_with_lock;
use crate::foundation::tail::{self, POLL_INTERVAL, RELOAD_EVERY_N_TICKS};

/// Per-channel merge state — one merged file + N slice files (one per
/// host that has at least one participant on this channel).
struct ChannelMergeState {
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

pub fn run(config_path: &Path, once: bool, quiet: bool) -> Result<()> {
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
    // v0.3.6: --quiet primarily exists for symmetry with `giga sync
    // --quiet` and to allow swarm_boss Monitors to opt into "errors
    // only" semantics. The startup status lines below DO emit even
    // under --quiet — they're one-shot, and the operator/agent benefits
    // from knowing the daemon woke up cleanly. Per-tick chatter is
    // already absent from merger (it only emits on errors).
    let qsuffix = if quiet { " (--quiet)" } else { "" };
    if tracked.is_empty() {
        eprintln!(
            "merger: no cross-host channels in {} — sitting idle, will reload config every ~{}s{qsuffix}",
            config_path.display(),
            POLL_INTERVAL.as_secs() * RELOAD_EVERY_N_TICKS,
        );
    } else {
        eprintln!(
            "merger: tracking {} cross-host channel(s): {}{qsuffix}",
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
        if tick.is_multiple_of(RELOAD_EVERY_N_TICKS) {
            refresh_tracked(config_path, &mut tracked, giga_home.as_deref());
        }
        merge_tick(&mut tracked, giga_home.as_deref());
    }
}

/// One merge sweep across all tracked channels + slices. Pure-ish (the
/// side effects are deterministic file I/O); extracted so tests can
/// invoke it without the 3s sleep loop.
fn merge_tick(tracked: &mut HashMap<String, ChannelMergeState>, giga_home: Option<&Path>) {
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
            let delta = match tail::read_delta(&slice.path, slice.last_size, cur) {
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

    let active = compute_active_channels(&cfg, cfg.this_host.as_deref());
    let active_names: HashSet<&str> = active.iter().map(|(n, _, _)| n.as_str()).collect();

    for (name, merged_path, slice_hosts) in &active {
        // Build/refresh per-channel state.
        let entry = tracked
            .entry(name.clone())
            .or_insert_with(|| ChannelMergeState {
                merged_path: merged_path.clone(),
                slices: HashMap::new(),
            });
        // Drop slices for hosts no longer participating.
        entry
            .slices
            .retain(|h, _| slice_hosts.iter().any(|sh| sh == h));
        // Add slices we don't have yet.
        for host in slice_hosts {
            if entry.slices.contains_key(host) {
                continue;
            }
            let slice_path = crate::foundation::slices::slice_path(merged_path, host);
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
///   (channel_filename, absolute_merged_path, sorted_distinct_PEER_slice_hosts)
///
/// v0.3.5 (REMOTE_DUAL_WRITE_DESIGN.md): excludes this_host from the
/// returned slice_hosts. Post dual-writes to the merged file directly
/// for cross-host channels, so the merger doesn't need to (and must
/// not, to avoid double-append) re-merge own slice into merged. Only
/// PEER slices need merging in.
fn compute_active_channels(
    cfg: &Config,
    this_host: Option<&str>,
) -> Vec<(String, PathBuf, Vec<String>)> {
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
            // v0.3.5: drop own host from the peer-slice list.
            if let Some(me) = this_host {
                hosts.retain(|h| h != me);
            }
            hosts.sort();
            hosts.dedup();
            Some((ch.file.clone(), merged_path, hosts))
        })
        .collect()
}

fn append_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    // v0.3.5: lock during append so concurrent post dual-writes to
    // this same merged file can't interleave bytes within a single
    // frame. See REMOTE_DUAL_WRITE_DESIGN.md §5 — POSIX O_APPEND is
    // atomic only up to PIPE_BUF (4KB), and merger ticks can carry
    // multi-frame deltas larger than that. Shared impl in foundation::append.
    append_with_lock(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    // Slice-path derivation is tested in foundation::slices.

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
    fn compute_active_channels_finds_cross_host_with_no_this_host_includes_all() {
        // No this_host context (legacy / pre-validation callsite) — return
        // every participating host. Useful as a sanity check on the filter
        // logic; production always passes this_host.
        let (_tmp, config_path, _inbox) = cross_host_fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        let active = compute_active_channels(&cfg, None);
        assert_eq!(active.len(), 1);
        let (name, _, hosts) = &active[0];
        assert_eq!(name, "alice-bob.md");
        assert_eq!(hosts, &vec!["wsl-a".to_string(), "wsl-b".to_string()]);
    }

    /// v0.3.5 T4 from REMOTE_DUAL_WRITE_DESIGN.md: when this_host is
    /// known, merger tracks PEER slices only. Own slice is owned by
    /// post (dual-write to merged) and re-merging it would double-append.
    #[test]
    fn compute_active_channels_excludes_this_host_from_slice_list() {
        let (_tmp, config_path, _inbox) = cross_host_fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        let active = compute_active_channels(&cfg, Some("wsl-a"));
        assert_eq!(active.len(), 1);
        let (_name, _, hosts) = &active[0];
        assert_eq!(hosts, &vec!["wsl-b".to_string()], "own host excluded");
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
        assert_eq!(compute_active_channels(&cfg, Some("wsl-a")).len(), 0);
    }

    #[test]
    fn merge_tick_appends_slice_growth_to_merged() {
        // v0.3.5: viewing as wsl-b so the wsl-a slice is the PEER slice
        // (merger's responsibility). Pre-v0.3.5 the test viewed as wsl-a
        // and merger absorbed its own slice; now own-slice is post's
        // dual-write responsibility.
        let (tmp, config_path, inbox) = cross_host_fixture("wsl-b");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));
        assert_eq!(tracked.len(), 1);

        // Write some content to the wsl-a (peer) slice as if alice posted.
        let slice_a = inbox.join("alice-bob.wsl-a.md");
        fs::write(
            &slice_a,
            b"\n\n===\n[alice] hi - 2026-01-01T00:00:00Z\n===\n",
        )
        .unwrap();

        merge_tick(&mut tracked, Some(tmp.path()));

        let merged = inbox.join("alice-bob.md");
        let body = fs::read_to_string(&merged).unwrap();
        assert!(body.contains("[alice] hi"));

        // Cursor should be advanced to the slice's current length.
        let cursor = cursor::read_merge(tmp.path(), "alice-bob.md", "wsl-a");
        assert_eq!(cursor, Some(fs::metadata(&slice_a).unwrap().len()));
    }

    #[test]
    fn merge_tick_merges_peer_slices_but_skips_own_slice() {
        // v0.3.5 invariant: merger merges PEER slices into merged. Own
        // slice is post's dual-write responsibility — merger touching
        // it would double-append. Viewing as wsl-a here; wsl-b is peer.
        let (tmp, config_path, inbox) = cross_host_fixture("wsl-a");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));

        let slice_a = inbox.join("alice-bob.wsl-a.md"); // OWN
        let slice_b = inbox.join("alice-bob.wsl-b.md"); // PEER
                                                        // Simulate: post on host wsl-a dual-wrote alice's frame already
                                                        // to merged (skip writing the merged file here so we can verify
                                                        // merger does NOT add the own-slice content a second time).
        fs::write(
            &slice_a,
            b"\n\n===\n[alice] own-via-post - 2026-01-01T00:00:00Z\n===\n",
        )
        .unwrap();
        // Peer slice arrives via sync.
        fs::write(
            &slice_b,
            b"\n\n===\n[bob] peer-via-sync - 2026-01-01T00:00:01Z\n===\n",
        )
        .unwrap();

        merge_tick(&mut tracked, Some(tmp.path()));

        let merged = fs::read_to_string(inbox.join("alice-bob.md")).unwrap();
        // Peer slice content lands in merged (merger's job).
        assert!(merged.contains("[bob] peer-via-sync"));
        // Own slice content does NOT land in merged via merger — it
        // would have been written directly by the post step (not exercised
        // here; the test seeded the slice file directly to isolate the
        // merger code path).
        assert!(
            !merged.contains("[alice] own-via-post"),
            "merger must skip own slice; post is responsible for the merged write"
        );
    }

    #[test]
    fn merge_tick_is_idempotent_when_no_growth() {
        // v0.3.5: view as wsl-b so wsl-a is the tracked peer slice.
        let (tmp, config_path, inbox) = cross_host_fixture("wsl-b");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));

        let slice_a = inbox.join("alice-bob.wsl-a.md");
        fs::write(
            &slice_a,
            b"\n\n===\n[alice] once - 2026-01-01T00:00:00Z\n===\n",
        )
        .unwrap();

        merge_tick(&mut tracked, Some(tmp.path()));
        merge_tick(&mut tracked, Some(tmp.path())); // no slice growth -> no-op
        merge_tick(&mut tracked, Some(tmp.path()));

        let merged = fs::read_to_string(inbox.join("alice-bob.md")).unwrap();
        // "once" should appear exactly once; no re-delivery.
        assert_eq!(merged.matches("[alice] once").count(), 1);
    }

    #[test]
    fn merge_tick_appends_incremental_growth() {
        // v0.3.5: view as wsl-b so wsl-a is the tracked peer slice.
        let (tmp, config_path, inbox) = cross_host_fixture("wsl-b");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));

        let slice_a = inbox.join("alice-bob.wsl-a.md");

        // First post.
        fs::write(
            &slice_a,
            b"\n\n===\n[alice] one - 2026-01-01T00:00:00Z\n===\n",
        )
        .unwrap();
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
        assert_eq!(
            merged.matches("[alice] one").count(),
            1,
            "no re-delivery on incremental tick"
        );
    }

    #[test]
    fn merge_tick_recovers_from_truncated_slice() {
        // Pathological: someone manually truncated a slice file. Merger
        // should reset its cursor and not panic; subsequent appends to
        // the slice get delivered fresh.
        // v0.3.5: view as wsl-b so wsl-a is the tracked peer slice.
        let (tmp, config_path, inbox) = cross_host_fixture("wsl-b");
        let mut tracked = HashMap::new();
        refresh_tracked(&config_path, &mut tracked, Some(tmp.path()));

        let slice_a = inbox.join("alice-bob.wsl-a.md");
        fs::write(
            &slice_a,
            b"\n\n===\n[alice] before - 2026-01-01T00:00:00Z\n===\n",
        )
        .unwrap();
        merge_tick(&mut tracked, Some(tmp.path()));

        // Truncate.
        fs::write(&slice_a, b"").unwrap();
        merge_tick(&mut tracked, Some(tmp.path())); // cursor reset, no append

        // Re-write fresh.
        fs::write(
            &slice_a,
            b"\n\n===\n[alice] after - 2026-01-01T00:00:02Z\n===\n",
        )
        .unwrap();
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
        assert_eq!(
            tracked.len(),
            0,
            "channel should be dropped from tracked set"
        );
    }
}
