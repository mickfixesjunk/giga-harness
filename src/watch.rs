//! `giga watch` — built-in inbox watcher.
//!
//! Two modes:
//!
//! * **Single-file (legacy):** `giga watch <channel> --as <agent>`.
//!   Polls one channel file every 3 seconds; prints `inbox: <line>` for
//!   every new header block whose sender is NOT `--as`. This is what
//!   the original bash + powershell watch-channel scripts did.
//!
//! * **Config-aware multi-channel:** `giga watch --as <agent> [--config <path>]`.
//!   Reads the config, tracks every channel where `<agent>` is a
//!   participant, polls all of them on the same 3-second tick, and
//!   periodically rereads the config so that channels added to it
//!   later (e.g. via `giga-add-agent`) get picked up without
//!   restarting the watcher. Emits `inbox <channel>: <line>` so the
//!   consumer can tell which channel fired.
//!
//! Both modes run forever; meant to be invoked under Claude Code's
//! Monitor tool with `persistent: true`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::cursor;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// How many poll ticks between config rereads. 5 ticks * 3s = ~15s,
/// which is a tight enough window that a freshly-added channel feels
/// "instant" without thrashing the disk.
const RELOAD_EVERY_N_TICKS: u64 = 5;

/// Single-file mode — legacy form, preserved for backward compat with
/// agents whose CLAUDE.md still spells out one Monitor per channel.
pub fn run_single(channel: &Path, me: &str) -> Result<()> {
    if !channel.exists() {
        anyhow::bail!("channel file not found: {}", channel.display());
    }
    let mut last = fs::metadata(channel)
        .with_context(|| format!("stat {}", channel.display()))?
        .len();
    let me_tag = format!("[{me}] ");
    loop {
        thread::sleep(POLL_INTERVAL);
        let cur = match fs::metadata(channel) {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if cur <= last {
            if cur < last {
                last = cur;
            }
            continue;
        }
        let delta = match read_delta(channel, last, cur) {
            Ok(d) => d,
            Err(_) => continue,
        };
        last = cur;
        for line in delta.lines() {
            if !is_header_line(line) {
                continue;
            }
            if line.starts_with(&me_tag) {
                continue;
            }
            println!("inbox: {line}");
        }
    }
}

/// Multi-channel mode — discovers every channel in the config where
/// `me` participates, tracks all of them, and rereads the config
/// every RELOAD_EVERY_N_TICKS ticks so new channels are picked up
/// automatically. Each channel starts from its stored read cursor
/// (written by `giga catchup` or a previous watch session) so the
/// agent is not re-notified about messages it has already seen.
/// Newly-discovered channels with no cursor fall back to current EOF.
pub fn run_multi(config_path: &Path, me: &str) -> Result<()> {
    if !config_path.exists() {
        anyhow::bail!(
            "config file not found: {} — pass --config <path>",
            config_path.display(),
        );
    }
    let giga_home = cursor::giga_home();
    let me_tag = format!("[{me}] ");
    let mut tracked: HashMap<String, ChannelState> = HashMap::new();
    let mut tick: u64 = 0;
    // Seed the file set immediately so we don't sit idle for the
    // first poll interval before discovering anything to watch.
    refresh_tracked(config_path, me, &mut tracked, giga_home.as_deref());
    if tracked.is_empty() {
        eprintln!(
            "watch: no channels in {} list `{me}` as a participant — sitting idle, will reload config every ~{}s",
            config_path.display(),
            POLL_INTERVAL.as_secs() * RELOAD_EVERY_N_TICKS,
        );
    } else {
        eprintln!(
            "watch: tracking {} channel(s) as `{me}`: {}",
            tracked.len(),
            tracked.keys().cloned().collect::<Vec<_>>().join(", "),
        );
    }
    loop {
        thread::sleep(POLL_INTERVAL);
        tick = tick.wrapping_add(1);
        if tick % RELOAD_EVERY_N_TICKS == 0 {
            refresh_tracked(config_path, me, &mut tracked, giga_home.as_deref());
        }
        for state in tracked.values_mut() {
            let cur = match fs::metadata(&state.path) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            if cur <= state.last_size {
                if cur < state.last_size {
                    state.last_size = cur;
                }
                continue;
            }
            let delta = match read_delta(&state.path, state.last_size, cur) {
                Ok(d) => d,
                Err(_) => continue,
            };
            state.last_size = cur;
            let mut notified = false;
            for line in delta.lines() {
                if !is_header_line(line) {
                    continue;
                }
                if line.starts_with(&me_tag) {
                    continue;
                }
                println!("inbox {}: {line}", state.name);
                notified = true;
            }
            // Advance the cursor after each batch of notifications so
            // the next session doesn't re-deliver messages from this one.
            if notified {
                if let Some(home) = &giga_home {
                    cursor::write(home, me, &state.name, state.last_size);
                }
            }
        }
    }
}

struct ChannelState {
    name: String,
    path: PathBuf,
    last_size: u64,
}

/// Reread the config and adjust the tracked set:
/// * add channels that now list `me` as a participant, starting from
///   the stored read cursor when one exists, or from byte 0 when no
///   cursor exists (first time this agent has watched this channel —
///   auto-replay history as notifications so the agent catches up on
///   anything posted while they were offline),
/// * drop channels that no longer do (or that were removed entirely).
///
/// Errors are logged to stderr but don't kill the watcher — a
/// transient config-edit race shouldn't take down the watcher.
fn refresh_tracked(
    config_path: &Path,
    me: &str,
    tracked: &mut HashMap<String, ChannelState>,
    giga_home: Option<&Path>,
) {
    let cfg = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("watch: config reload failed ({e}) — keeping current channel set");
            return;
        }
    };
    let active: Vec<(String, PathBuf)> = cfg
        .channels
        .iter()
        .filter(|c| c.participants.iter().any(|p| p == me))
        .filter_map(|c| match cfg.channel_path(c) {
            Ok(p) => Some((c.file.clone(), p)),
            Err(e) => {
                eprintln!("watch: skip channel `{}` — {e}", c.file);
                None
            }
        })
        .collect();
    let active_names: HashSet<String> = active.iter().map(|(n, _)| n.clone()).collect();

    for (name, path) in &active {
        if tracked.contains_key(name) {
            continue;
        }
        let eof = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        // Use the stored cursor if one exists; otherwise start from
        // byte 0 so the first watch session on a channel replays the
        // whole history as notifications and the agent gets caught up.
        // After the first poll tick advances the cursor to EOF, future
        // sessions only see new messages.
        let start = giga_home
            .and_then(|home| cursor::read(home, me, name))
            .unwrap_or(0);
        tracked.insert(
            name.clone(),
            ChannelState {
                name: name.clone(),
                path: path.clone(),
                last_size: start,
            },
        );
        if start < eof {
            eprintln!(
                "watch: catching up on `{name}` ({} unread bytes)",
                eof - start,
            );
        } else {
            eprintln!("watch: tracking `{name}` (at EOF, no backlog)");
        }
    }
    let to_drop: Vec<String> = tracked
        .keys()
        .filter(|k| !active_names.contains(*k))
        .cloned()
        .collect();
    for name in to_drop {
        tracked.remove(&name);
        eprintln!("watch: dropped channel `{name}` (no longer a participant)");
    }
}

fn is_header_line(line: &str) -> bool {
    // Header blocks look like `[sender] subject — UTC-ISO-8601-timestamp`.
    // Filter on the cheap `[` prefix + `] ` separator.
    line.starts_with('[') && line.contains("] ")
}

fn read_delta(path: &Path, from: u64, to: u64) -> Result<String> {
    let mut f = fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0u8; (to - from) as usize];
    f.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}
