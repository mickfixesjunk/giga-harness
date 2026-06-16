//! Broadcast-prefix classification for the watcher.
//!
//! This relocates the watcher's `extract_subject` helper and gives the
//! `[fyi]`/`[ack: …]`/`[all]`/`[giga-rearm]` classification a named home
//! ([`classify`]). The actual delivery / archive / stagger behavior stays
//! in `watch::run_multi` — this module is purely the "what kind of
//! broadcast is this header line?" decision, unchanged from before.

use crate::config::{self, BroadcastPrefix};

/// v0.4.0: extract the subject text from a header line like
/// `[design 2026-06-01 12:00 PST] [ack: alice] cleanup nudge — 2026-06-01T12:00:00Z`.
/// Returns the slice between the first `]` (closing the agent/timestamp
/// prefix the watcher already validated) and the trailing ISO timestamp
/// or end-of-line. `parse_broadcast_prefix` then scans that subject.
pub(super) fn extract_subject(header_line: &str) -> &str {
    // Header convention: `[<sender> <ts>] <subject> — <iso8601>`
    // We want everything after the first `]`. The broadcast-prefix
    // parser is robust to trailing whitespace.
    match header_line.find(']') {
        Some(idx) => header_line[idx + 1..].trim_start(),
        None => header_line,
    }
}

/// Classify a broadcast-channel header line by its subject prefix.
///
/// This is exactly `config::parse_broadcast_prefix(extract_subject(line))`
/// — the existing decision, just given a name. `None` means "no special
/// prefix" (treated identically to `[all]` by the caller). The caller
/// (`watch::run_multi`) still owns all the resulting behavior: the
/// `[fyi]` archive, the `[giga-rearm]` self-rearm, the `[ack]`/`[all]`
/// stagger math, and self-address filtering.
pub(super) fn classify(header_line: &str) -> Option<BroadcastPrefix> {
    config::parse_broadcast_prefix(extract_subject(header_line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_subject_strips_sender_prefix() {
        let line = "[design 2026-06-01 12:00 PST] [ack: alice] nudge — 2026-06-01T12:00:00Z";
        assert_eq!(
            extract_subject(line),
            "[ack: alice] nudge — 2026-06-01T12:00:00Z"
        );
    }

    #[test]
    fn extract_subject_no_bracket_returns_whole_line() {
        assert_eq!(extract_subject("no bracket here"), "no bracket here");
    }

    #[test]
    fn classify_matches_parse_broadcast_prefix_of_subject() {
        let line = "[design] [fyi] heads up — 2026-06-01T12:00:00Z";
        assert_eq!(
            classify(line),
            config::parse_broadcast_prefix("[fyi] heads up — 2026-06-01T12:00:00Z")
        );
    }
}
