//! Stale-wait detection — scan a channel for unresolved
//! `WAITING ON: <me>` tags and emit them at watcher arm time.
//!
//! The watcher fires once per new message past the cursor, then
//! advances. If an agent's session compacts or the agent misses the
//! wakeup, a `WAITING ON: <me>` becomes invisible: nothing re-fires
//! it. Sender stays quiet (per protocol — blocked agents wait), and
//! the wedge stays silent until someone manually greps channels.
//!
//! This module fixes that by re-deriving pending waits from channel
//! state at arm time. One emission per session start, one line per
//! unresolved wait — caught here, before the agent re-arms its
//! session and falls back into idle.
//!
//! Resolution semantics (per the feature spec):
//! * A WAITING ON: <me> tag from sender S is resolved when:
//!   - The receiver (me) posts ANY message on the same channel after
//!     the tag, OR
//!   - S posts a superseding WAITING ON: <other>, OR
//!   - S posts an `(Informational, no response required.)` closure.
//! * A new WAITING ON: <me> from S supersedes S's prior wait — only
//!   the latest per (channel, sender) pair matters.
//! * Messages without a clear footer are no-ops for state tracking
//!   (malformed tags silently ignored, per the spec).
//!
//! v1 scope: scan ONCE at watcher arm time. No periodic re-fire, no
//! resolution state machine maintained beyond the single scan. The
//! compaction-loss case (which was the most expensive failure mode
//! in the two incidents that prompted this feature) is fully covered
//! by the one-shot scan.

use chrono::{DateTime, NaiveDateTime, Utc};
use std::collections::HashMap;
use std::path::Path;

/// One unresolved wait surfaced to the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleWait {
    /// The agent slug that posted the WAITING ON: <me> tag.
    pub sender: String,
    /// Subject of the message carrying the tag (stripped of the
    /// trailing ` — <timestamp>` so the operator sees what they see
    /// in the inbox).
    pub subject: String,
    /// The header timestamp on the message that carries the tag.
    pub tag_timestamp: DateTime<Utc>,
    /// Convenience: how many minutes between `tag_timestamp` and the
    /// `now` passed to `scan`. Always non-negative; if the tag is in
    /// the future (clock skew between hosts), returns 0.
    pub age_minutes: u64,
}

/// Scan a channel's full content for unresolved WAITING ON: <me>
/// tags. Returns one entry per sender whose latest wait on me is
/// older than `threshold_minutes` AND has not been resolved.
///
/// Pure function: takes content + clock-now and returns a Vec —
/// no I/O, deterministic, easy to test.
pub fn scan(
    content: &str,
    me: &str,
    now: DateTime<Utc>,
    threshold_minutes: u64,
) -> Vec<StaleWait> {
    // Per-sender state: their LATEST unresolved WAITING ON: me with
    // its timestamp + subject. Overwritten on supersede; removed on
    // resolution.
    let mut pending: HashMap<String, (DateTime<Utc>, String)> = HashMap::new();

    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if let Some((sender, subject, ts)) = parse_header(lines[i]) {
            let footer = find_footer_in_message(&lines, i + 1);
            if sender == me {
                // Receiver posted → resolves every pending wait on
                // this channel, regardless of who was waiting.
                pending.clear();
            } else {
                match footer {
                    Some(Footer::WaitingOn(target)) if target == me => {
                        // New WAITING ON: me from sender — supersedes
                        // their prior wait (if any), or starts tracking.
                        pending.insert(sender, (ts, subject));
                    }
                    Some(Footer::WaitingOn(_)) => {
                        // Sender now waiting on someone else — moved on,
                        // clear their pending wait on me.
                        pending.remove(&sender);
                    }
                    Some(Footer::Informational) => {
                        // Sender closed informationally — released.
                        pending.remove(&sender);
                    }
                    None => {
                        // No clear footer — malformed or convention
                        // pre-WAITING-ON era. Leave state untouched.
                    }
                }
            }
        }
        i += 1;
    }

    // Convert the remaining (per-sender) pending map into a vec of
    // StaleWait, filtered to those past the threshold.
    let mut out: Vec<StaleWait> = pending
        .into_iter()
        .filter_map(|(sender, (ts, subject))| {
            let age = now.signed_duration_since(ts).num_minutes();
            let age_minutes = if age < 0 { 0 } else { age as u64 };
            if age_minutes >= threshold_minutes {
                Some(StaleWait {
                    sender,
                    subject,
                    tag_timestamp: ts,
                    age_minutes,
                })
            } else {
                None
            }
        })
        .collect();
    // Stable order: oldest waits first (operator sees the worst-
    // wedged first), tiebreak by sender slug.
    out.sort_by(|a, b| {
        b.age_minutes
            .cmp(&a.age_minutes)
            .then_with(|| a.sender.cmp(&b.sender))
    });
    out
}

/// Convenience wrapper for use from `giga watch`. Reads the file
/// (best-effort) and scans. Returns an empty Vec if the file is
/// missing or unreadable — the watcher should never crash on a
/// stale-wait scan failure.
pub fn scan_file(
    path: &Path,
    me: &str,
    now: DateTime<Utc>,
    threshold_minutes: u64,
) -> Vec<StaleWait> {
    match std::fs::read_to_string(path) {
        Ok(body) => scan(&body, me, now, threshold_minutes),
        Err(_) => Vec::new(),
    }
}

/// Format a stale-wait notification line for emission to stderr at
/// watcher arm time. Matches the spec example:
///
///   inbox <channel>: ⏰ STALE WAIT 47m — [sender] <subject>
pub fn format_notification(channel: &str, wait: &StaleWait) -> String {
    format!(
        "inbox {channel}: ⏰ STALE WAIT {age}m — [{sender}] {subject}",
        age = wait.age_minutes,
        sender = wait.sender,
        subject = wait.subject,
    )
}

// --- parsing helpers -----------------------------------------------

/// Parse a header line of the form `[<sender>] <subject> — <UTC ISO ts>`.
/// Returns (sender, subject, ts) on success.
///
/// The timestamp is the LAST 20 ASCII bytes of the line. The subject
/// is everything between the closing `]` and the ` — <timestamp>`
/// separator. We split from the right with rsplitn so an em-dash in
/// the subject (which is common — agents use em-dashes in subjects)
/// doesn't confuse the parser.
fn parse_header(line: &str) -> Option<(String, String, DateTime<Utc>)> {
    if !line.starts_with('[') {
        return None;
    }
    // Skip the `[<placeholder>]` example header in the channel
    // preamble — matches is_header_line in watch.rs.
    if line.starts_with("[<") {
        return None;
    }
    let bracket_end = line.find("] ")?;
    let sender = line[1..bracket_end].to_string();
    if sender.is_empty() {
        return None;
    }
    let after_bracket = &line[bracket_end + 2..];
    // Split from the right on ` — ` (space + em-dash + space).
    let mut rsplit = after_bracket.rsplitn(2, " — ");
    let ts_str = rsplit.next()?;
    let subject = rsplit.next()?;
    // Timestamp must be exactly 20 ASCII bytes ending in Z.
    if ts_str.len() != 20 || !ts_str.ends_with('Z') {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%dT%H:%M:%SZ").ok()?;
    let ts = naive.and_utc();
    Some((sender, subject.to_string(), ts))
}

enum Footer {
    WaitingOn(String),
    Informational,
}

/// Walk forward from `start` until we hit either a footer or the
/// next message header. Returns None if neither is found (the
/// message is malformed — treat as no-op upstream).
fn find_footer_in_message(lines: &[&str], start: usize) -> Option<Footer> {
    for line in lines.iter().skip(start) {
        if parse_header(line).is_some() {
            // Next message started without a footer in the previous
            // one — give up on the previous one.
            return None;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("WAITING ON: ") {
            let who = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
            if who.is_empty() {
                return None;
            }
            return Some(Footer::WaitingOn(who.to_string()));
        }
        if line.contains("Informational, no response required") {
            return Some(Footer::Informational);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
            .unwrap()
            .and_utc()
    }

    fn msg(sender: &str, subject: &str, ts_str: &str, footer: &str) -> String {
        format!(
            "===\n[{sender}] {subject} — {ts_str}\n===\n\nbody\n\n{footer}\n===\n\n"
        )
    }

    /// Basic case: sender posts WAITING ON: me, no response, past
    /// threshold. Should emerge.
    #[test]
    fn scan_emits_single_unresolved_wait_past_threshold() {
        let ch = msg(
            "alice",
            "PR #43 ready for review",
            "2026-06-05T00:00:00Z",
            "WAITING ON: bob (review)",
        );
        let now = ts("2026-06-05T00:47:00Z");
        let out = scan(&ch, "bob", now, 30);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sender, "alice");
        assert_eq!(out[0].subject, "PR #43 ready for review");
        assert_eq!(out[0].age_minutes, 47);
    }

    /// Below threshold → not emitted. Spec default is 30min.
    #[test]
    fn scan_suppresses_wait_below_threshold() {
        let ch = msg(
            "alice",
            "PR #43",
            "2026-06-05T00:00:00Z",
            "WAITING ON: bob (review)",
        );
        let now = ts("2026-06-05T00:15:00Z");
        let out = scan(&ch, "bob", now, 30);
        assert!(out.is_empty(), "15m wait shouldn't surface under a 30m threshold: {out:?}");
    }

    /// Receiver responding clears the wait.
    #[test]
    fn receiver_response_resolves_wait() {
        let ch = format!(
            "{}{}",
            msg("alice", "PR #43", "2026-06-05T00:00:00Z", "WAITING ON: bob (review)"),
            msg("bob", "reviewed", "2026-06-05T00:10:00Z", "(Informational, no response required.)"),
        );
        let now = ts("2026-06-05T01:00:00Z");
        let out = scan(&ch, "bob", now, 30);
        assert!(out.is_empty(), "receiver response should resolve: {out:?}");
    }

    /// Sender informational closure resolves the wait.
    #[test]
    fn sender_informational_closure_resolves_wait() {
        let ch = format!(
            "{}{}",
            msg("alice", "PR #43", "2026-06-05T00:00:00Z", "WAITING ON: bob (review)"),
            msg("alice", "nvm, merged it", "2026-06-05T00:05:00Z", "(Informational, no response required.)"),
        );
        let now = ts("2026-06-05T01:00:00Z");
        let out = scan(&ch, "bob", now, 30);
        assert!(out.is_empty(), "sender informational should resolve: {out:?}");
    }

    /// Sender pivoting to a different recipient resolves the wait on
    /// the original target.
    #[test]
    fn sender_pivot_to_other_recipient_resolves_original_wait() {
        let ch = format!(
            "{}{}",
            msg("alice", "PR #43", "2026-06-05T00:00:00Z", "WAITING ON: bob (review)"),
            msg("alice", "actually carol can review", "2026-06-05T00:05:00Z", "WAITING ON: carol (review)"),
        );
        let now = ts("2026-06-05T01:00:00Z");
        let out = scan(&ch, "bob", now, 30);
        assert!(out.is_empty(), "sender pivoting away should resolve bob's wait: {out:?}");
    }

    /// Sender re-posting WAITING ON: me SUPERSEDES with new timestamp.
    /// Only the latest per-sender entry survives.
    #[test]
    fn sender_repost_supersedes_with_latest_timestamp() {
        let ch = format!(
            "{}{}",
            msg("alice", "first ping", "2026-06-05T00:00:00Z", "WAITING ON: bob (review)"),
            msg("alice", "second ping", "2026-06-05T00:20:00Z", "WAITING ON: bob (still)"),
        );
        let now = ts("2026-06-05T01:00:00Z");
        let out = scan(&ch, "bob", now, 30);
        assert_eq!(out.len(), 1);
        // Latest message wins — subject and age reflect the second one.
        assert_eq!(out[0].subject, "second ping");
        assert_eq!(out[0].age_minutes, 40); // 60 - 20
    }

    /// Multiple distinct senders → one entry each.
    #[test]
    fn multiple_senders_each_get_own_entry() {
        let ch = format!(
            "{}{}",
            msg("alice", "thing A", "2026-06-05T00:00:00Z", "WAITING ON: bob (review)"),
            msg("carol", "thing C", "2026-06-05T00:10:00Z", "WAITING ON: bob (review)"),
        );
        let now = ts("2026-06-05T01:00:00Z");
        let out = scan(&ch, "bob", now, 30);
        assert_eq!(out.len(), 2);
        // Oldest first (alice's 60m before carol's 50m).
        assert_eq!(out[0].sender, "alice");
        assert_eq!(out[0].age_minutes, 60);
        assert_eq!(out[1].sender, "carol");
        assert_eq!(out[1].age_minutes, 50);
    }

    /// A WAITING ON pointing at someone else (not me) is ignored.
    #[test]
    fn wait_on_other_agent_is_ignored() {
        let ch = msg(
            "alice",
            "PR #43",
            "2026-06-05T00:00:00Z",
            "WAITING ON: carol (review)",
        );
        let now = ts("2026-06-05T01:00:00Z");
        let out = scan(&ch, "bob", now, 30);
        assert!(out.is_empty());
    }

    /// Empty channel → no waits.
    #[test]
    fn empty_channel_returns_empty() {
        assert!(scan("", "bob", ts("2026-06-05T00:00:00Z"), 30).is_empty());
    }

    /// Future-dated tag (clock skew) → age=0, suppressed under any
    /// positive threshold. Tolerates without panicking.
    #[test]
    fn future_dated_tag_does_not_panic_and_clamps_age_to_zero() {
        let ch = msg(
            "alice",
            "from the future",
            "2026-06-05T02:00:00Z",
            "WAITING ON: bob (review)",
        );
        let now = ts("2026-06-05T00:00:00Z");
        // age clamped to 0 → not past any threshold > 0.
        let out = scan(&ch, "bob", now, 30);
        assert!(out.is_empty());
        // Zero threshold surfaces everything including future tags.
        let out0 = scan(&ch, "bob", now, 0);
        assert_eq!(out0.len(), 1);
        assert_eq!(out0[0].age_minutes, 0);
    }

    /// Message with no footer → no state change. Prior pending wait
    /// survives across an empty message from a different agent.
    #[test]
    fn message_without_footer_leaves_state_untouched() {
        let ch = format!(
            "{}{}",
            msg("alice", "ping", "2026-06-05T00:00:00Z", "WAITING ON: bob (review)"),
            // carol posts something with no footer (malformed/old-convention).
            "===\n[carol] random — 2026-06-05T00:10:00Z\n===\n\nbody only no footer\n===\n\n",
        );
        let now = ts("2026-06-05T01:00:00Z");
        let out = scan(&ch, "bob", now, 30);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sender, "alice");
    }

    /// Em-dashes in the subject must not break the parser (rsplitn
    /// on the LAST ` — ` keeps the subject intact).
    #[test]
    fn em_dash_in_subject_does_not_confuse_parser() {
        let line = "[alice] big — refactor — review request — 2026-06-05T00:00:00Z";
        let parsed = parse_header(line).expect("should parse");
        assert_eq!(parsed.0, "alice");
        assert_eq!(parsed.1, "big — refactor — review request");
        assert_eq!(
            parsed.2,
            NaiveDateTime::parse_from_str("2026-06-05T00:00:00Z", "%Y-%m-%dT%H:%M:%SZ")
                .unwrap()
                .and_utc()
        );
    }

    /// The placeholder `[<sender>] ...` convention preamble must NOT
    /// be parsed as a real header.
    #[test]
    fn placeholder_header_is_not_parsed() {
        assert!(parse_header("[<sender>] <subject> — <timestamp>").is_none());
    }

    /// format_notification matches the spec example shape.
    #[test]
    fn format_notification_matches_spec_shape() {
        let wait = StaleWait {
            sender: "alice".to_string(),
            subject: "PR #43 ready for review".to_string(),
            tag_timestamp: ts("2026-06-05T00:00:00Z"),
            age_minutes: 47,
        };
        let line = format_notification("alice-bob.md", &wait);
        assert_eq!(
            line,
            "inbox alice-bob.md: ⏰ STALE WAIT 47m — [alice] PR #43 ready for review"
        );
        // Operators grep on the text marker:
        assert!(line.contains("STALE WAIT"));
    }

    /// Receiver response to ONE channel resolves all pending waits on
    /// THAT channel — three senders pending, then me posts, all clear.
    #[test]
    fn receiver_post_resolves_all_senders_on_channel() {
        let ch = format!(
            "{}{}{}{}",
            msg("alice", "A", "2026-06-05T00:00:00Z", "WAITING ON: bob (x)"),
            msg("carol", "C", "2026-06-05T00:01:00Z", "WAITING ON: bob (y)"),
            msg("dave", "D", "2026-06-05T00:02:00Z", "WAITING ON: bob (z)"),
            msg("bob", "ack all", "2026-06-05T00:30:00Z", "(Informational, no response required.)"),
        );
        let now = ts("2026-06-05T01:00:00Z");
        let out = scan(&ch, "bob", now, 30);
        assert!(out.is_empty(), "receiver's single post should clear all pending: {out:?}");
    }

    /// `scan_file` returns empty on missing file (never crashes the
    /// watcher).
    #[test]
    fn scan_file_returns_empty_for_missing_path() {
        let out = scan_file(
            Path::new("/nonexistent-stale-wait-test"),
            "bob",
            ts("2026-06-05T00:00:00Z"),
            30,
        );
        assert!(out.is_empty());
    }
}
