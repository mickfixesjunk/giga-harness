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
use std::io::Write; // Stdout::flush (agy force-flush mode)
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

use crate::config::{self, BroadcastPrefix, Config};
use crate::cursor;
use crate::foundation::frame;
use crate::foundation::tail::{self, POLL_INTERVAL, RELOAD_EVERY_N_TICKS};

/// v0.6.0: watch delivery mode. Default = Claude (stdout lines for
/// Monitor tool). `--agy` = stdout lines + flush + exit-on-WAITING-ON-me.
/// `--codex` = write JSON envelopes to `$CODEX_CHANNEL_DIR/inbox/`.
/// Mutually exclusive at the CLI level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    Default,
    Agy,
    Codex,
}

// POLL_INTERVAL (3s) and RELOAD_EVERY_N_TICKS (5 ticks ≈ 15s, tight
// enough that a freshly-added channel feels "instant" without thrashing
// the disk) are imported from foundation::tail — the single source shared
// with the merger and codex bridge.

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
    let Ok(meta) = fs::metadata(lock) else {
        return false;
    };
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
pub fn run_single(channel: &Path, me: &str, mode: WatchMode) -> Result<()> {
    if matches!(mode, WatchMode::Codex) {
        return Err(anyhow!(
            "--codex requires multi-channel mode (omit the positional CHANNEL arg) — \
             single-channel codex watching isn't supported; the bridge needs access to all channels"
        ));
    }
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
            if let Ok(delta) = tail::read_delta_lossy(channel, last, cur) {
                last = cur;
                for line in delta.lines() {
                    if !frame::is_header_line(line) {
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
            if matches!(mode, WatchMode::Agy) {
                let _ = std::io::stdout().flush();
                if is_waiting_on_me(channel, me) {
                    eprintln!("watch [agy]: WAITING ON `{me}` detected → exit 0");
                    std::process::exit(0);
                }
            }
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
pub fn run_multi(
    config_path: &Path,
    me: &str,
    stagger_override: Option<u64>,
    mode: WatchMode,
) -> Result<()> {
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

    // v0.6.0: --agy implies --no-stagger. AGY's reactive-wakeup model
    // doesn't benefit from staggering (no TPM-burst risk per slot;
    // delayed delivery defeats the wake-on-task-exit signal).
    let effective_stagger_override = if matches!(mode, WatchMode::Agy) {
        Some(0)
    } else {
        stagger_override
    };

    // v0.6.0: --codex needs $CODEX_CHANNEL_DIR pointing at the per-agent
    // bridge directory (created by `giga init` for codex agents).
    // Validate up-front so we fail loud on the operator's pane instead
    // of silently dropping envelopes.
    let codex_inbox = if matches!(mode, WatchMode::Codex) {
        let dir = std::env::var("CODEX_CHANNEL_DIR").map_err(|_| {
            anyhow!(
                "--codex requires CODEX_CHANNEL_DIR env var (path to the agent's codex-channel/ dir). \
                 `giga launch` sets this automatically for codex agents."
            )
        })?;
        let inbox = PathBuf::from(dir).join("inbox");
        if !inbox.exists() {
            return Err(anyhow!(
                "codex inbox dir not found: {} — run `giga init` to scaffold it",
                inbox.display(),
            ));
        }
        Some(inbox)
    } else {
        None
    };

    // Swarm name (for envelope `swarm` field) loaded once.
    let swarm_name = Config::load(config_path)
        .map(|c| c.project.name.clone())
        .unwrap_or_else(|_| "unknown".to_string());

    // v0.4.0: resolve the broadcast stagger value. CLI > TOML > default.
    // v0.6.2: default bumped to 30 (was 15) to halve peak TPM during
    // broadcast fanout; matches BroadcastConfig::default_broadcast_stagger.
    let stagger_seconds = match effective_stagger_override {
        Some(v) => v,
        None => Config::load(config_path)
            .map(|c| c.broadcast.stagger_seconds)
            .unwrap_or(30),
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
    eprintln!("watch: broadcast stagger = {stagger_seconds}s (0 = instant fanout)");
    // v0.6.2 diagnostic: per-broadcast-channel, print this agent's slot
    // and the expected delay so the operator can verify stagger is
    // engaged WITHOUT waiting for a broadcast to fire. If this section
    // is missing from a watcher's startup log, the binary is pre-v0.6.2
    // (or pre-v0.4.0 if even the previous "broadcast stagger" line is
    // missing) — that's the diagnostic for "is stagger actually
    // engaging or are we silently all firing at once?".
    let mut broadcast_states: Vec<(&str, u64)> = tracked
        .values()
        .filter(|s| config::is_broadcast_channel(&s.name))
        .map(|s| {
            let recipients: Vec<&str> = s.participants.iter().map(|p| p.as_str()).collect();
            let delay = config::fanout_delay_seconds(me, &recipients, stagger_seconds);
            (s.name.as_str(), delay)
        })
        .collect();
    broadcast_states.sort_by_key(|(name, _)| *name);
    for (name, delay) in &broadcast_states {
        let total = tracked
            .get(*name)
            .map(|s| s.participants.len())
            .unwrap_or(0);
        eprintln!(
            "watch: broadcast `{name}` → this agent's slot delay = {delay}s ({total} participants)",
        );
    }

    // v0.6.16: stale-wait scan at arm time. Re-derives unresolved
    // `WAITING ON: <me>` tags from each tracked channel's content so
    // a compaction-loss or missed-wakeup case surfaces as an
    // immediate notification instead of staying silently wedged.
    //
    // v0.6.17: dedup state shared with the periodic re-scan inside
    // the loop below. Keyed by (channel, sender, tag_timestamp) so
    // the same stale wait fires at most one Monitor notification per
    // supersede — zero LLM cost beyond first detection.
    let mut surfaced_waits: std::collections::HashSet<(
        String,
        String,
        chrono::DateTime<chrono::Utc>,
    )> = std::collections::HashSet::new();
    let rescan_seconds = run_stale_wait_scan(
        config_path,
        me,
        &tracked,
        &mut surfaced_waits,
        /* announce_recheck = */ true,
    );
    // Convert the rescan cadence into a per-tick interval (POLL_INTERVAL = 3s).
    let rescan_every_n_ticks: u64 = if rescan_seconds == 0 {
        0 // disabled
    } else {
        std::cmp::max(1, rescan_seconds / POLL_INTERVAL.as_secs())
    };

    loop {
        thread::sleep(POLL_INTERVAL);
        tick = tick.wrapping_add(1);
        if tick % RELOAD_EVERY_N_TICKS == 0 {
            refresh_tracked(config_path, me, &mut tracked, giga_home.as_deref());
        }
        // v0.6.17: periodic stale-wait re-scan. Cheap (local file I/O,
        // no LLM cost) and dedup'd via `surfaced_waits` so a stale
        // wait fires at most one Monitor notification per supersede.
        // Catches the cases the arm-time scan can't: agent alive but
        // missed the original Monitor (busy turn, compaction mid-
        // session), a wait that crossed the threshold AFTER arm time,
        // and a mid-turn API kill where the agent restarted into a
        // new watcher session.
        if rescan_every_n_ticks > 0 && tick % rescan_every_n_ticks == 0 {
            run_stale_wait_scan(
                config_path,
                me,
                &tracked,
                &mut surfaced_waits,
                /* announce_recheck = */ false,
            );
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
            let delta = match tail::read_delta_lossy(&state.path, state.last_size, cur) {
                Ok(d) => d,
                Err(_) => continue,
            };
            state.last_size = cur;
            let is_broadcast = config::is_broadcast_channel(&state.name);
            for line in delta.lines() {
                if !frame::is_header_line(line) {
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
                    Some(BroadcastPrefix::GigaRearm) => {
                        // v0.6.3: silent watcher self-rearm. Advance
                        // the cursor past this message FIRST so the
                        // new watcher (loaded from disk after exec)
                        // doesn't re-process the rearm broadcast and
                        // ping-pong infinitely. Then exec self with
                        // the same args — POSIX execve replaces the
                        // process in-place, Monitor's stdout pipe
                        // stays connected to the same PID running
                        // new code, and the agent's Claude session
                        // is never woken. Zero API calls.
                        eprintln!(
                            "watch: [giga-rearm] received on `{}` → cursor advanced + execing self",
                            state.name,
                        );
                        if let Some(home) = &giga_home {
                            cursor::write(home, me, &state.name, state.last_size);
                        }
                        self_rearm();
                        // self_rearm() doesn't return on success; if
                        // exec failed we keep handling the broadcast
                        // as if it were [all] (fall through to wake-up).
                    }
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
                        let recipients: Vec<&str> = addressed.iter().map(|s| s.as_str()).collect();
                        let delay = config::fanout_delay_seconds(me, &recipients, stagger_seconds);
                        let ready_at = now + Duration::from_secs(delay);
                        // v0.6.2 diagnostic: per-broadcast log the
                        // deferral so the operator can confirm stagger
                        // engaged on this specific message.
                        eprintln!(
                            "watch: broadcast on `{}` [ack] → deferring {}s (slot {} of {})",
                            state.name,
                            delay,
                            slot_for(me, &recipients),
                            recipients.len(),
                        );
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
                        // v0.6.2 diagnostic: per-broadcast log the
                        // deferral. If you see "deferring 0s slot 0"
                        // for every agent in a swarm, stagger is NOT
                        // engaging (Possibility B from the rate-limit
                        // diagnosis). If you see varying delays per
                        // agent, stagger IS engaging — the issue is
                        // just per-turn TPM cost, fix by increasing
                        // [broadcast].stagger_seconds further.
                        eprintln!(
                            "watch: broadcast on `{}` [all] → deferring {}s (slot {} of {})",
                            state.name,
                            delay,
                            slot_for(me, &recipients),
                            recipients.len(),
                        );
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
        let mut should_exit_for_agy = false;
        for state in tracked.values_mut() {
            if state.pending.is_empty() {
                continue;
            }
            let mut still_pending: Vec<(Instant, String)> = Vec::new();
            for (ready_at, line) in state.pending.drain(..) {
                if ready_at <= now {
                    // v0.6.0: dispatch on watch mode.
                    match mode {
                        WatchMode::Default => {
                            println!("{line}");
                        }
                        WatchMode::Agy => {
                            println!("{line}");
                            // Force-flush so AGY's stdout-stream
                            // delivers immediately (no line-buffering).
                            let _ = std::io::stdout().flush();
                            // If the channel's latest message is
                            // WAITING ON us, exit 0 — triggers AGY's
                            // task-completion wakeup with the action
                            // already delivered.
                            if is_waiting_on_me(&state.path, me) {
                                should_exit_for_agy = true;
                            }
                        }
                        WatchMode::Codex => {
                            // Write a brief envelope into the codex
                            // inbox dir. The codex CLI picks it up,
                            // surfaces it to the agent, and writes a
                            // receipt to the outbox.
                            if let Some(inbox) = &codex_inbox {
                                let text = format!(
                                    "Giga inbox notification for `{me}`.\n\n\
                                     Channel: {channel}\n\
                                     Path: {path}\n\
                                     Header: {line}\n\n\
                                     Read the channel file, follow your agent instructions, \
                                     and respond via `giga post` if the message requires action.",
                                    channel = state.name,
                                    path = state.path.display(),
                                );
                                if let Err(e) = crate::codex_channel::write_envelope(
                                    inbox,
                                    &swarm_name,
                                    me,
                                    &state.name,
                                    state.last_size,
                                    &text,
                                ) {
                                    eprintln!("watch [codex]: envelope write failed: {e:#}");
                                }
                            }
                        }
                    }
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
        // v0.6.0: AGY exit-on-WAITING-ON happens AFTER the cursor write
        // so the persisted state reflects what we delivered. The next
        // re-armed `giga watch --agy` resumes from the right offset.
        if should_exit_for_agy {
            eprintln!("watch [agy]: WAITING ON `{me}` detected → exiting 0 to fire AGY task-completion wakeup");
            std::process::exit(0);
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

#[cfg(test)]
mod tests {
    use super::*;

    // Header-detection tests now live with the parser in
    // `foundation::frame` (the watcher self-filter is `frame::is_header_line`).

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

    /// v0.6.0: is_waiting_on_me returns true when the LATEST message
    /// on the channel has a `WAITING ON: <me>` footer.
    #[test]
    fn is_waiting_on_me_detects_direct_addressed_footer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("ch.md");
        fs::write(&path, "===\n[design] hello — 2026-06-02T00:00:00Z\n===\n\nbody\n\nWAITING ON: research (status)\n===\n").unwrap();
        assert!(is_waiting_on_me(&path, "research"));
        assert!(!is_waiting_on_me(&path, "design"));
    }

    /// v0.6.0: informational footer means NO ONE is waited on.
    #[test]
    fn is_waiting_on_me_returns_false_for_informational() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("ch.md");
        fs::write(&path, "===\n[design] FYI — 2026-06-02T00:00:00Z\n===\n\nbody\n\n(Informational, no response required.)\n===\n").unwrap();
        assert!(!is_waiting_on_me(&path, "research"));
        assert!(!is_waiting_on_me(&path, "design"));
    }

    /// v0.6.0: only the LAST message matters — older WAITING ON lines
    /// don't trigger if a subsequent message is informational or
    /// addresses someone else.
    #[test]
    fn is_waiting_on_me_only_considers_latest_message() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("ch.md");
        fs::write(
            &path,
            "===\n[design] first — 2026-06-02T00:00:00Z\n===\n\nbody\n\nWAITING ON: research (status)\n===\n\n\
             ===\n[research] reply — 2026-06-02T00:01:00Z\n===\n\nbody\n\n(Informational, no response required.)\n===\n",
        )
        .unwrap();
        // The LATEST message is informational → research is no longer waited on.
        assert!(!is_waiting_on_me(&path, "research"));
    }

    /// v0.6.0: missing file = no, not panic.
    #[test]
    fn is_waiting_on_me_returns_false_for_missing_file() {
        assert!(!is_waiting_on_me(
            Path::new("/nonexistent/__giga_test"),
            "anyone"
        ));
    }
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
    let ts = crate::foundation::timefmt::now_iso8601();
    let line = format!("[{ts}] {channel}: {header}\n");
    // v0.6.x: route through the locked append. Previously this used an
    // UNLOCKED OpenOptions::append, which could tear against a concurrent
    // writer to the same archive.
    if let Err(e) = crate::foundation::append::append_with_lock(&archive_path, line.as_bytes()) {
        eprintln!(
            "watch: failed to append FYI to {} ({e}) — message will not be surfaced",
            archive_path.display(),
        );
    }
}

/// v0.6.0: scan the channel file's LAST header block for a
/// `WAITING ON: <agent>` line. Returns true when the latest message
/// is actively addressed to `me`. Used by `--agy` mode to decide
/// whether to exit cleanly (firing AGY's task-completion wakeup).
///
/// Naive single-target parse: matches the first non-`<` token on the
/// WAITING ON line. Tolerates surrounding punctuation. Multi-target
/// "WAITING ON: a, b" is treated as "waiting on a" (good enough for
/// v1; refinement deferred to multi-target spec if it ships).
/// v0.6.16 (arm-time) + v0.6.17 (periodic re-scan): run a stale-wait
/// scan across every tracked channel and emit one Monitor-shaped
/// notification per UNSEEN unresolved `WAITING ON: <me>` past
/// threshold. `surfaced_waits` is mutated in place to:
///   - skip re-emission of waits already surfaced this session
///     (keyed by channel + sender + tag_timestamp, so a supersede
///     with a new timestamp DOES re-fire — that's the desired
///     behavior, sender posted again)
///   - drop stale entries no longer in the scan results (resolution
///     happened) so the set doesn't grow unbounded
///
/// Returns the watcher's recheck-cadence in seconds so the caller
/// can compute the per-tick interval. Returns 0 on config-load
/// failure (caller treats as "disabled"). When `announce_recheck`
/// is true, the cadence line is printed alongside the standard
/// arm-time summary; when false (in-loop calls), only the actual
/// stale waits are printed.
fn run_stale_wait_scan(
    config_path: &Path,
    me: &str,
    tracked: &HashMap<String, ChannelState>,
    surfaced_waits: &mut std::collections::HashSet<(String, String, chrono::DateTime<chrono::Utc>)>,
    announce_recheck: bool,
) -> u64 {
    let Ok(cfg) = Config::load(config_path) else {
        return 0;
    };
    let global_threshold = cfg.watch.stale_wait_threshold_minutes;
    let recheck_seconds = cfg.watch.stale_wait_recheck_seconds;
    let now = chrono::Utc::now();

    let mut per_channel_threshold: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    for ch in &cfg.channels {
        if let Some(t) = ch.stale_wait_threshold_minutes {
            per_channel_threshold.insert(ch.file.clone(), t);
        }
    }

    let mut total_new = 0usize;
    let mut current_keys: std::collections::HashSet<(
        String,
        String,
        chrono::DateTime<chrono::Utc>,
    )> = std::collections::HashSet::new();
    // Deterministic order so emission is stable across re-arms.
    let mut names: Vec<&str> = tracked.keys().map(|s| s.as_str()).collect();
    names.sort();
    for name in names {
        let Some(state) = tracked.get(name) else {
            continue;
        };
        let threshold = per_channel_threshold
            .get(name)
            .copied()
            .unwrap_or(global_threshold);
        let waits = crate::stale_wait::scan_file(&state.path, me, now, threshold);
        for w in &waits {
            let key = (name.to_string(), w.sender.clone(), w.tag_timestamp);
            current_keys.insert(key.clone());
            if surfaced_waits.insert(key) {
                eprintln!("{}", crate::stale_wait::format_notification(name, w));
                total_new += 1;
            }
        }
    }
    // Drop resolved waits from the dedup set so a future SUPERSEDE
    // (same sender posts a new WAITING ON: me at a new timestamp)
    // would re-emit. The (channel, sender, ts) key already enforces
    // supersede-on-new-ts semantics, but we still want to bound the
    // set's memory growth over a long watcher lifetime.
    surfaced_waits.retain(|k| current_keys.contains(k));

    if announce_recheck {
        if recheck_seconds == 0 {
            eprintln!(
                "watch: stale-wait re-scan disabled (`[watch].stale_wait_recheck_seconds = 0`); arm-time scan only"
            );
        } else {
            eprintln!(
                "watch: stale-wait re-scan every {recheck_seconds}s (threshold {global_threshold}m default; per-channel overrides applied where set)"
            );
        }
        if total_new > 0 {
            eprintln!("watch: {total_new} stale wait(s) surfaced above");
        }
    }

    recheck_seconds
}

fn is_waiting_on_me(path: &Path, me: &str) -> bool {
    let Ok(body) = fs::read_to_string(path) else {
        return false;
    };
    // The latest frame's footer is `WAITING ON: <me>` (informational
    // synonyms and a different target both read as "not waiting on me").
    matches!(
        frame::last_header_block(&body).as_ref().and_then(|lf| lf.waiting_on()),
        Some(who) if who == me
    )
}

/// v0.6.2: compute the agent's slot index in the alphabetically-sorted
/// recipient list. Mirror of `config::fanout_delay_seconds`'s slot
/// computation but returns the slot number (for diagnostic logging)
/// rather than the slot × stagger product.
fn slot_for(this_agent: &str, recipients: &[&str]) -> usize {
    let mut sorted: Vec<&str> = recipients.to_vec();
    sorted.sort();
    sorted.iter().position(|a| *a == this_agent).unwrap_or(0)
}

/// v0.6.3: replace the running watcher process with a fresh `giga
/// watch` invocation that loads the new binary from disk.
///
/// On POSIX, `execve(2)` reuses the same process slot — PID, open
/// file descriptors, stdio pipes — and replaces the program text +
/// heap. The Monitor task that spawned us sees no exit; its pipe
/// stays connected to the new code reading the same channels. The
/// agent's Claude session is genuinely never woken. Zero API calls
/// across the whole upgrade-rearm path.
///
/// On Windows, exec-in-place isn't available; we fall back to spawn
/// + exit. Monitor sees the parent die and reports it (which costs
/// an API call to the agent), but the agent's next turn can re-arm
/// from CLAUDE.md / AGENTS.md as before. Worse than POSIX, but
/// matches today's behavior on Windows.
///
/// We rebuild the argv from `std::env::args()` so flags like
/// `--as`, `--codex`, `--agy`, `--stagger-seconds`, `--config` are
/// preserved verbatim across the rearm.
fn self_rearm() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "watch: self-rearm failed to resolve current_exe ({e}) — \
                 falling through to wake-up rearm"
            );
            return;
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    eprintln!(
        "watch: self-rearm → exec {} {}",
        exe.display(),
        args.join(" ")
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec() only returns on FAILURE. On success, the current
        // process is replaced — anything below this line never runs.
        let err = std::process::Command::new(&exe).args(&args).exec();
        eprintln!("watch: self-rearm exec failed ({err}) — falling through to wake-up rearm");
    }
    #[cfg(not(unix))]
    {
        // Windows: no in-place exec. Spawn + exit. Monitor will see
        // the parent die; the agent's next turn must re-arm via the
        // CLAUDE.md/AGENTS.md instructions (today's wake-up flow).
        let _ = std::process::Command::new(&exe).args(&args).spawn();
        std::process::exit(0);
    }
}
