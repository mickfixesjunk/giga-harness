//! Broadcast message-semantics: subject-prefix parsing, the
//! `_*.md` channel convention, and the staggered-fanout slot
//! computation. See BROADCAST_FANOUT_DESIGN.md.

/// v0.4.0: parsed shape of a broadcast subject's leading prefix. Used
/// by `watch.rs` to decide what to do with a notification on a `_*.md`
/// channel. See BROADCAST_FANOUT_DESIGN.md §3.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastPrefix {
    /// `[fyi]` — informational. Watcher logs to per-agent archive
    /// instead of firing Monitor notification (zero LLM cost).
    Fyi,
    /// `[ack: a, b, c]` — fire only for agents in the list.
    Ack(Vec<String>),
    /// `[all]` or no prefix — fire for every participant (with
    /// staggered fanout from `BroadcastConfig.stagger_seconds`).
    All,
    /// v0.6.3: `[giga-rearm]` — silent watcher-rebinary signal.
    /// Watcher writes its cursor past this message, then POSIX-execve's
    /// itself with the same args. New binary loads from disk; Monitor
    /// task sees no exit; agent's Claude session is never woken.
    /// Zero API calls swarm-wide. `giga upgrade` posts with this
    /// prefix as of v0.6.3. Pre-v0.6.3 watchers parse this as None →
    /// fall back to `All` (wake-up rearm) — backward compat for the
    /// first upgrade ONTO v0.6.3.
    GigaRearm,
}

/// Parse the leading broadcast prefix out of a subject line. Tolerant
/// of whitespace; case-insensitive on the prefix tag. Returns `None`
/// for the unprefixed case (caller treats as `All` when
/// `default_recipients = "all"`). The prefix may appear AFTER the
/// existing `[<agent> YYYY-MM-DD HH:MM PST]` convention header — the
/// parser scans past the timestamp-shaped first prefix when present.
pub fn parse_broadcast_prefix(subject: &str) -> Option<BroadcastPrefix> {
    let rest = strip_timestamp_prefix(subject.trim_start());
    let rest = rest.trim_start();
    if !rest.starts_with('[') {
        return None;
    }
    let end = rest.find(']')?;
    let inside = rest[1..end].trim();
    if inside.is_empty() {
        return None;
    }
    let lower = inside.to_ascii_lowercase();
    if lower == "fyi" {
        return Some(BroadcastPrefix::Fyi);
    }
    if lower == "all" {
        return Some(BroadcastPrefix::All);
    }
    if lower == "giga-rearm" {
        return Some(BroadcastPrefix::GigaRearm);
    }
    // `[ack: a, b, c]` form. Split on first `:`.
    if let Some(colon) = inside.find(':') {
        let tag = inside[..colon].trim().to_ascii_lowercase();
        if tag == "ack" {
            let recipients: Vec<String> = inside[colon + 1..]
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            return Some(BroadcastPrefix::Ack(recipients));
        }
    }
    None
}

/// If the subject begins with `[<word> YYYY-MM-DD HH:MM <TZ>]`, return
/// the slice AFTER that prefix. Otherwise return the input. This is
/// the convention header agents already use; broadcast prefixes
/// appear AFTER it, so the parser needs to skip past.
fn strip_timestamp_prefix(s: &str) -> &str {
    if !s.starts_with('[') {
        return s;
    }
    let Some(end) = s.find(']') else {
        return s;
    };
    let inside = &s[1..end];
    // Heuristic: contains a date-like `YYYY-MM-DD` substring AND
    // doesn't look like one of our broadcast tags. Cheap + good enough.
    let looks_like_timestamp = inside.chars().filter(|c| *c == '-').count() >= 2
        && inside.chars().any(|c| c.is_ascii_digit());
    let lower = inside.trim().to_ascii_lowercase();
    let is_broadcast_tag = lower == "fyi"
        || lower == "all"
        || lower == "giga-rearm"
        || lower.starts_with("ack:")
        || lower.starts_with("ack ");
    if looks_like_timestamp && !is_broadcast_tag {
        return &s[end + 1..];
    }
    s
}

/// True for channel filenames that match the broadcast convention
/// (`_*.md`). Used by `watch.rs` to decide whether broadcast-specific
/// fanout handling applies.
pub fn is_broadcast_channel(filename: &str) -> bool {
    filename.starts_with('_') && filename.ends_with(".md")
}

/// Compute the stable per-agent fanout delay slot for a broadcast.
/// Slot = position of `this_agent` in the alphabetically-sorted
/// recipient list. Same agent always gets the same slot (deterministic
/// across watcher restarts). See BROADCAST_FANOUT_DESIGN.md §3.2.
pub fn fanout_delay_seconds(this_agent: &str, recipients: &[&str], stagger_seconds: u64) -> u64 {
    if stagger_seconds == 0 {
        return 0;
    }
    let mut sorted: Vec<&str> = recipients.to_vec();
    sorted.sort();
    let slot = sorted.iter().position(|a| *a == this_agent).unwrap_or(0) as u64;
    slot * stagger_seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_config_stagger_zero_disables_fanout_delay() {
        assert_eq!(fanout_delay_seconds("alice", &["alice", "bob"], 0), 0);
        assert_eq!(fanout_delay_seconds("bob", &["alice", "bob"], 0), 0);
    }

    /// v0.6.3: `[giga-rearm]` triggers the silent watcher self-rearm
    /// path. parse_broadcast_prefix returns the new variant so
    /// watch.rs can dispatch on it.
    #[test]
    fn parse_broadcast_prefix_recognizes_giga_rearm() {
        assert_eq!(
            parse_broadcast_prefix("[giga-rearm] giga upgraded"),
            Some(BroadcastPrefix::GigaRearm)
        );
        assert_eq!(
            parse_broadcast_prefix("[GIGA-REARM] case insensitive"),
            Some(BroadcastPrefix::GigaRearm)
        );
        // After timestamp wrapper.
        assert_eq!(
            parse_broadcast_prefix("[design 2026-06-02 12:00 PST] [giga-rearm] please"),
            Some(BroadcastPrefix::GigaRearm),
        );
    }

    #[test]
    fn parse_broadcast_prefix_recognizes_fyi() {
        assert_eq!(
            parse_broadcast_prefix("[fyi] host-c came online"),
            Some(BroadcastPrefix::Fyi)
        );
        assert_eq!(
            parse_broadcast_prefix("[FYI] case insensitive"),
            Some(BroadcastPrefix::Fyi)
        );
        assert_eq!(
            parse_broadcast_prefix("  [ fyi ]  whitespace tolerant"),
            Some(BroadcastPrefix::Fyi)
        );
    }

    #[test]
    fn parse_broadcast_prefix_recognizes_ack_list() {
        let parsed = parse_broadcast_prefix("[ack: alice, bob, carol] cleanup nudge");
        match parsed {
            Some(BroadcastPrefix::Ack(list)) => {
                assert_eq!(list, vec!["alice", "bob", "carol"]);
            }
            other => panic!("expected Ack, got {other:?}"),
        }
    }

    #[test]
    fn parse_broadcast_prefix_recognizes_all() {
        assert_eq!(
            parse_broadcast_prefix("[all] hello everyone"),
            Some(BroadcastPrefix::All)
        );
    }

    #[test]
    fn parse_broadcast_prefix_returns_none_for_unprefixed() {
        assert_eq!(parse_broadcast_prefix("plain subject no brackets"), None);
        assert_eq!(parse_broadcast_prefix("[unknown-tag] something"), None);
    }

    #[test]
    fn parse_broadcast_prefix_skips_timestamp_header() {
        // The convention from CLAUDE.md is "[<agent> YYYY-MM-DD HH:MM PST]".
        // The parser must skip past that to find the broadcast prefix.
        let parsed =
            parse_broadcast_prefix("[design 2026-06-01 12:00 PST] [ack: alice] cleanup nudge");
        match parsed {
            Some(BroadcastPrefix::Ack(list)) => assert_eq!(list, vec!["alice"]),
            other => panic!("expected Ack after timestamp prefix, got {other:?}"),
        }
    }

    #[test]
    fn parse_broadcast_prefix_handles_fyi_after_timestamp() {
        assert_eq!(
            parse_broadcast_prefix("[design 2026-06-01 12:00 PST] [fyi] foo"),
            Some(BroadcastPrefix::Fyi),
        );
    }

    #[test]
    fn parse_broadcast_prefix_empty_ack_list_yields_empty_vec() {
        let parsed = parse_broadcast_prefix("[ack: ] empty list");
        match parsed {
            Some(BroadcastPrefix::Ack(list)) => assert!(list.is_empty()),
            other => panic!("expected Ack, got {other:?}"),
        }
    }

    #[test]
    fn is_broadcast_channel_matches_underscore_prefix() {
        assert!(is_broadcast_channel("_broadcast.md"));
        assert!(is_broadcast_channel("_announcements.md"));
        assert!(!is_broadcast_channel("alice-bob.md"));
        assert!(!is_broadcast_channel("broadcast.md"));
        assert!(!is_broadcast_channel("_broadcast.txt"));
    }

    #[test]
    fn fanout_delay_assigns_stable_slots() {
        let agents = ["bob", "alice", "carol"];
        // Sorted: alice (0), bob (1), carol (2). Stagger 10s.
        assert_eq!(fanout_delay_seconds("alice", &agents, 10), 0);
        assert_eq!(fanout_delay_seconds("bob", &agents, 10), 10);
        assert_eq!(fanout_delay_seconds("carol", &agents, 10), 20);
    }

    #[test]
    fn fanout_delay_for_unknown_agent_defaults_to_zero_slot() {
        let agents = ["alice", "bob"];
        // Unknown agent gets slot 0 (no delay) — conservative; the
        // caller already filtered the recipient list.
        assert_eq!(fanout_delay_seconds("eve", &agents, 10), 0);
    }
}
