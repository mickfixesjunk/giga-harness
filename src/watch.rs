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
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::config::{self, BroadcastPrefix, Config};
use crate::cursor;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// How many poll ticks between config rereads. 5 ticks * 3s = ~15s,
/// which is a tight enough window that a freshly-added channel feels
/// "instant" without thrashing the disk.
const RELOAD_EVERY_N_TICKS: u64 = 5;

/// A busy-lock older than this is treated as idle (flush). This is the
/// fail-safe for a missed unlock: if an agent's turn crashes before its
/// Stop hook removes the lock, the watcher must NOT buffer forever and
/// go permanently deaf — after this window it flushes anyway. Generous
/// because legitimate agentic turns can run minutes; a turn quieter than
/// this with no lock refresh is pathological, and flushing then is the
/// safe choice.
const BUSY_LOCK_STALE_AFTER: Duration = Duration::from_secs(300);

/// Path of the per-agent busy-lock. An agent's turn-start hook
/// (`UserPromptSubmit` / `PreToolUse`) touches this file; its `Stop`
/// hook removes it. While it is present and fresh, `giga watch` BUFFERS
/// notifications instead of emitting them — so a queued inbox event is
/// never spliced onto an in-flight assistant turn. (Doing so modifies
/// the latest assistant message's interleaved-thinking blocks, which the
/// Anthropic API rejects with a 400 "thinking blocks ... cannot be
/// modified", permanently wedging the session.) Buffered events flush at
/// the next idle tick — between turns, the safe boundary.
///
/// Returns None when there's no giga home, which disables gating
/// entirely: with no lock the watcher behaves exactly as before, so this
/// is a no-op unless the hooks are installed (opt-in, zero default change).
fn busy_lock_path(giga_home: Option<&Path>, me: &str) -> Option<PathBuf> {
    giga_home.map(|h| h.join("busy").join(format!("{me}.lock")))
}

/// True only when the lock exists AND is fresher than the stale window.
/// Biased toward NOT-busy (flush) on every uncertainty — an unreadable
/// mtime, a stale lock, or no lock at all all resolve to "idle". Deafness
/// is the catastrophic failure mode here; an occasional unprotected flush
/// is not. So we never let lock-state ambiguity buffer events forever.
fn agent_is_busy(lock: Option<&PathBuf>) -> bool {
    let Some(lock) = lock else { return false };
    let Ok(meta) = fs::metadata(lock) else { return false };
    match meta.modified() {
        Ok(mtime) => mtime
            .elapsed()
            .map(|age| age < BUSY_LOCK_STALE_AFTER)
            .unwrap_or(false),
        Err(_) => false,
    }
}

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
    let lock = busy_lock_path(cursor::giga_home().as_deref(), me);
    let mut pending: Vec<String> = Vec::new();
    loop {
        thread::sleep(POLL_INTERVAL);
        let cur = match fs::metadata(channel) {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if cur < last {
            last = cur;
        } else if cur > last {
            if let Ok(delta) = read_delta(channel, last, cur) {
                last = cur;
                for line in delta.lines() {
                    if !is_header_line(line) {
                        continue;
                    }
                    if line.starts_with(&me_tag) {
                        continue;
                    }
                    pending.push(format!("inbox: {line}"));
                }
            }
        }
        // Same busy-lock gate as multi-channel mode: hold notifications
        // while the agent's turn is in flight, flush them when idle.
        if agent_is_busy(lock.as_ref()) {
            continue;
        }
        for line in pending.drain(..) {
            println!("{line}");
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
pub fn run_multi(config_path: &Path, me: &str, stagger_override: Option<u64>) -> Result<()> {
    if !config_path.exists() {
        anyhow::bail!(
            "config file not found: {} — pass --config <path>",
            config_path.display(),
        );
    }
    let giga_home = cursor::giga_home();
    let lock = busy_lock_path(giga_home.as_deref(), me);
    let me_tag = format!("[{me}] ");
    let mut tracked: HashMap<String, ChannelState> = HashMap::new();
    let mut tick: u64 = 0;

    // v0.4.0: resolve the broadcast stagger value. CLI > TOML > 15s default.
    let stagger_seconds = match stagger_override {
        Some(v) => v,
        None => Config::load(config_path)
            .map(|c| c.broadcast.stagger_seconds)
            .unwrap_or(15),
    };

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
    eprintln!(
        "watch: broadcast stagger = {stagger_seconds}s (0 = instant fanout)"
    );
    loop {
        thread::sleep(POLL_INTERVAL);
        tick = tick.wrapping_add(1);
        if tick % RELOAD_EVERY_N_TICKS == 0 {
            refresh_tracked(config_path, me, &mut tracked, giga_home.as_deref());
        }
        // Phase 1 — read new content into each channel's pending buffer.
        // We advance the in-memory read position (last_size) as we consume
        // bytes, but do NOT emit or persist the cursor here: emission is
        // gated on the agent being idle (phase 2).
        //
        // v0.4.0: for broadcast channels (`_*.md`), apply prefix
        // filtering ([fyi] / [ack: ...] / [all]) and compute a
        // staggered "do-not-fire-before" Instant per pending entry.
        let now = Instant::now();
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
            let is_broadcast = config::is_broadcast_channel(&state.name);
            for line in delta.lines() {
                if !is_header_line(line) {
                    continue;
                }
                if line.starts_with(&me_tag) {
                    continue;
                }
                // Non-broadcast: surface immediately (today's behavior).
                if !is_broadcast {
                    state
                        .pending
                        .push((now, format!("inbox {}: {line}", state.name)));
                    continue;
                }
                // Broadcast channel: parse subject prefix for filtering + stagger.
                let subject = extract_subject(line);
                match config::parse_broadcast_prefix(subject) {
                    Some(BroadcastPrefix::Fyi) => {
                        // Archive; don't fire.
                        if let Some(home) = &giga_home {
                            append_fyi_archive(home, me, &state.name, line);
                        }
                    }
                    Some(BroadcastPrefix::Ack(addressed)) => {
                        if !addressed.iter().any(|a| a == me) {
                            continue; // not addressed to us
                        }
                        // Staggered slot within the addressed set.
                        let recipients: Vec<&str> =
                            addressed.iter().map(|s| s.as_str()).collect();
                        let delay = config::fanout_delay_seconds(me, &recipients, stagger_seconds);
                        let ready_at = now + Duration::from_secs(delay);
                        state
                            .pending
                            .push((ready_at, format!("inbox {}: {line}", state.name)));
                    }
                    Some(BroadcastPrefix::All) | None => {
                        // Stagger across all channel participants.
                        let recipients: Vec<&str> =
                            state.participants.iter().map(|s| s.as_str()).collect();
                        let delay = config::fanout_delay_seconds(me, &recipients, stagger_seconds);
                        let ready_at = now + Duration::from_secs(delay);
                        state
                            .pending
                            .push((ready_at, format!("inbox {}: {line}", state.name)));
                    }
                }
            }
        }

        // Phase 2 — flush pending notifications ONLY when the agent is
        // idle. While the busy-lock is held, queued events stay buffered
        // so they're never spliced onto an in-flight (interleaved-thinking)
        // assistant turn. When idle, they flush together — between turns,
        // the safe boundary — and only THEN is the cursor persisted, so a
        // crash while buffered re-delivers rather than loses.
        //
        // v0.4.0: an entry only flushes when ready_at <= now. Entries
        // whose stagger window hasn't elapsed stay in pending for a
        // future tick. The persisted cursor advances per channel to
        // last_size only when ALL entries for that channel have flushed
        // — otherwise we'd lose the un-flushed ones on a watcher
        // restart.
        if agent_is_busy(lock.as_ref()) {
            continue;
        }
        let now = Instant::now();
        for state in tracked.values_mut() {
            if state.pending.is_empty() {
                continue;
            }
            let mut still_pending: Vec<(Instant, String)> = Vec::new();
            for (ready_at, line) in state.pending.drain(..) {
                if ready_at <= now {
                    println!("{line}");
                } else {
                    still_pending.push((ready_at, line));
                }
            }
            state.pending = still_pending;
            if state.pending.is_empty() {
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
    /// v0.4.0: sorted participant list for this channel, captured at
    /// refresh_tracked time. Used to compute the stable per-agent
    /// fanout slot for broadcast channels.
    participants: Vec<String>,
    /// Notifications read from the channel but not yet emitted, because
    /// the agent was busy when they arrived. Flushed at the next idle
    /// tick. The persisted cursor is NOT advanced until these are
    /// actually emitted, so a crash while buffered re-delivers them next
    /// session (re-delivery is safe; loss is not).
    ///
    /// v0.4.0: each entry carries a "do-not-fire-before" Instant. For
    /// non-broadcast channels this equals Instant::now() at push time
    /// (immediate). For broadcast channels with `[all]` or no prefix,
    /// it's pushed forward by `slot × stagger_seconds` to smooth the
    /// per-account API rate-limit hit across the recipient set.
    pending: Vec<(Instant, String)>,
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
    let active: Vec<(String, PathBuf, Vec<String>)> = cfg
        .channels
        .iter()
        .filter(|c| c.participants.iter().any(|p| p == me))
        .filter_map(|c| match cfg.channel_path(c) {
            Ok(p) => {
                let mut sorted_parts = c.participants.clone();
                sorted_parts.sort();
                Some((c.file.clone(), p, sorted_parts))
            }
            Err(e) => {
                eprintln!("watch: skip channel `{}` — {e}", c.file);
                None
            }
        })
        .collect();
    let active_names: HashSet<String> = active.iter().map(|(n, _, _)| n.clone()).collect();

    for (name, path, participants) in &active {
        if let Some(state) = tracked.get_mut(name) {
            // v0.4.0: refresh the participants list in case it changed
            // (add-agent appended a participant; new agent will get its
            // own slot starting next broadcast).
            state.participants = participants.clone();
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
                participants: participants.clone(),
                pending: Vec::new(),
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
    if !line.starts_with('[') || !line.contains("] ") {
        return false;
    }
    // Channel files include a literal example header in the convention
    // preamble with a `<sender>` placeholder — filter that out.
    if line.starts_with("[<") {
        return false;
    }
    // Real headers always end with a UTC timestamp produced by
    // `%Y-%m-%dT%H:%M:%SZ` — exactly 20 ASCII bytes, e.g.
    // `2026-05-28T14:30:00Z`. Body lines that open with `[agent] —`
    // (agents addressing the recipient inline) don't have this tail
    // and would otherwise leak past the --as self-filter, causing echo
    // notifications.
    //
    // Index the WHOLE line's bytes (`as_bytes()`), NOT a `&str` byte-slice
    // like `line[line.len()-20..]` — the latter panics when the 20-bytes-
    // from-end boundary lands inside a multibyte char (e.g. an em-dash in
    // the subject/body). The timestamp tail is pure ASCII, so checking the
    // last 20 bytes is correct regardless of multibyte chars earlier in the line.
    let bytes = line.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    let tail = &bytes[bytes.len() - 20..];
    tail[19] == b'Z'
        && tail[4] == b'-'
        && tail[7] == b'-'
        && tail[10] == b'T'
        && tail[13] == b':'
        && tail[16] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_header_passes() {
        assert!(is_header_line(
            "[design] online — 2026-05-28T14:30:00Z"
        ));
    }

    #[test]
    fn body_line_addressing_recipient_is_rejected() {
        // This is the echo-bug trigger: agent body opens with [recipient] —
        assert!(!is_header_line("[web] — Mick's explicit GO for the new feature"));
        assert!(!is_header_line("[superdeduper] — first: v0.2.29 bench results"));
    }

    #[test]
    fn multibyte_char_at_tail_boundary_does_not_panic() {
        // Regression: `line[line.len()-20..]` panicked when the 20-bytes-from-end
        // boundary fell inside a multibyte char (em-dash). A body line ending with
        // em-dashes near the tail must be rejected WITHOUT panicking.
        assert!(!is_header_line(
            "[superdeduper] — relocate the FULL stack (NOT a feature — fits the freeze)."
        ));
        // Em-dash exactly straddling the 20-from-end boundary.
        assert!(!is_header_line("[design] aaaaaaaaaaaaaaaa — bbbbbbbbbbbbbbbb"));
        // A real header with an em-dash in the subject still passes (ASCII tail intact).
        assert!(is_header_line(
            "[design] bench — results — 2026-05-28T14:30:00Z"
        ));
    }

    #[test]
    fn preamble_placeholder_is_rejected() {
        assert!(!is_header_line("[<sender>] <subject> — <UTC...>"));
    }

    #[test]
    fn non_bracket_line_is_rejected() {
        assert!(!is_header_line("just some body text"));
        assert!(!is_header_line("==="));
        assert!(!is_header_line("WAITING ON: web"));
    }

    #[test]
    fn header_with_em_dash_in_subject_passes() {
        // Subject itself may contain em-dashes — still valid.
        assert!(is_header_line(
            "[design] bench — results — 2026-05-28T14:30:00Z"
        ));
    }

    #[test]
    fn busy_when_no_giga_home_is_never_busy() {
        // No home -> no lock path -> gating disabled -> behaves as before.
        assert!(!agent_is_busy(busy_lock_path(None, "design").as_ref()));
    }

    #[test]
    fn busy_when_lock_absent_is_idle() {
        let dir = std::env::temp_dir().join(format!("giga-watch-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let lock = busy_lock_path(Some(&dir), "design");
        // busy/design.lock does not exist -> idle.
        assert!(!agent_is_busy(lock.as_ref()));
    }

    #[test]
    fn busy_when_fresh_lock_present() {
        let dir = std::env::temp_dir().join(format!("giga-watch-busy-{}", std::process::id()));
        let busy = dir.join("busy");
        fs::create_dir_all(&busy).unwrap();
        let lock = busy_lock_path(Some(&dir), "design").unwrap();
        fs::write(&lock, b"").unwrap(); // just-created -> fresh
        assert!(agent_is_busy(Some(&lock)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn busy_when_stale_lock_is_idle() {
        // A lock older than the stale window must read as idle (flush), so
        // a missed Stop-hook can't make the agent permanently deaf.
        let dir = std::env::temp_dir().join(format!("giga-watch-stale-{}", std::process::id()));
        let busy = dir.join("busy");
        fs::create_dir_all(&busy).unwrap();
        let lock = busy_lock_path(Some(&dir), "design").unwrap();
        fs::write(&lock, b"").unwrap();
        // Backdate mtime well past BUSY_LOCK_STALE_AFTER.
        let stale = std::time::SystemTime::now() - BUSY_LOCK_STALE_AFTER - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(stale)
            .unwrap();
        assert!(!agent_is_busy(Some(&lock)));
        let _ = fs::remove_dir_all(&dir);
    }
}

fn read_delta(path: &Path, from: u64, to: u64) -> Result<String> {
    let mut f = fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0u8; (to - from) as usize];
    f.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// v0.4.0: extract the subject text from a header line like
/// `[design 2026-06-01 12:00 PST] [ack: alice] cleanup nudge — 2026-06-01T12:00:00Z`.
/// Returns the slice between the first `]` (closing the agent/timestamp
/// prefix the watcher already validated) and the trailing ISO timestamp
/// or end-of-line. `parse_broadcast_prefix` then scans that subject.
fn extract_subject(header_line: &str) -> &str {
    // Header convention: `[<sender> <ts>] <subject> — <iso8601>`
    // We want everything after the first `]`. The broadcast-prefix
    // parser is robust to trailing whitespace.
    let after_first = match header_line.find(']') {
        Some(idx) => header_line[idx + 1..].trim_start(),
        None => header_line,
    };
    after_first
}

/// v0.4.0: append a `[fyi]` broadcast to a per-agent local archive
/// instead of firing it as a Monitor notification (BROADCAST_FANOUT_DESIGN.md
/// Idea C). Best-effort — failures are logged to stderr but don't
/// affect the watch loop.
fn append_fyi_archive(giga_home: &Path, agent: &str, channel: &str, header: &str) {
    let archive_path = giga_home.join(format!("fyi-archive.{agent}.log"));
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] {channel}: {header}\n");
    let result = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&archive_path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
    if let Err(e) = result {
        eprintln!(
            "watch: failed to append FYI to {} ({e}) — message will not be surfaced",
            archive_path.display(),
        );
    }
}
