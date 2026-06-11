//! Parser for giga channel-file posts.
//!
//! A channel file is an append-only `.md` log where each post is a
//! triple-equals-delimited block:
//!
//! ```text
//! ===
//! [<sender>] <subject> — <UTC-ISO-8601-timestamp>
//! ===
//!
//! <body lines>
//!
//! ===
//! ```
//!
//! The opening / closing `===` between posts is shared (one post's
//! closing `===` is the next post's opening). Headers are detected
//! using the same rule the `giga watch` watcher uses
//! (`watch::is_header_line`): starts with `[`, ends with a 20-byte
//! UTC timestamp tail. Body is everything between the header's
//! closing `===` and the next `===` (or EOF).
//!
//! Phase C scope (v0.6.34): structured post extraction for the
//! channel-tail REST endpoint. Phase D layers a per-channel live
//! tailer on top of the same parser.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Post {
    /// Sender slug — content of the first `[...]` group on the
    /// header line (e.g. `design`, `superdeduper`, `codex-review`).
    pub sender: String,
    /// Subject text — the readable middle portion of the header,
    /// minus the sender prefix and the trailing
    /// ` — <UTC-timestamp>`. Includes any inner `[YYYY-MM-DD HH:MM
    /// TZ]` annotation the sender chose to put there.
    pub subject: String,
    /// UTC ISO-8601 timestamp tail (e.g. `2026-06-05T20:43:01Z`).
    /// Always 20 ASCII chars when present; empty when the header
    /// is malformed (shouldn't happen for files written by giga
    /// itself).
    pub timestamp_iso: String,
    /// Post body — everything between the header's closing `===`
    /// and the post's closing `===`, with surrounding blank lines
    /// trimmed.
    pub body: String,
}

/// Parse all posts in a channel-file's text content, oldest first.
/// Use `.iter().rev().take(n)` (or similar) at the call site to get
/// the most-recent N.
pub fn parse(content: &str) -> Vec<Post> {
    let lines: Vec<&str> = content.lines().collect();
    let mut posts = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != "===" {
            i += 1;
            continue;
        }
        // Need a header line at i+1 and a closing `===` at i+2.
        let header_idx = i + 1;
        let body_start = i + 3;
        if body_start > lines.len() {
            break;
        }
        if !is_header_line(lines[header_idx]) || lines[i + 2].trim() != "===" {
            i += 1;
            continue;
        }
        // Body runs from i+3 until the next standalone `===`.
        let mut j = body_start;
        let body_end = loop {
            if j >= lines.len() {
                break lines.len();
            }
            if lines[j].trim() == "===" {
                break j;
            }
            j += 1;
        };
        let body = lines[body_start..body_end].join("\n").trim().to_string();
        let (sender, subject, ts) = parse_header(lines[header_idx]);
        posts.push(Post {
            sender,
            subject,
            timestamp_iso: ts,
            body,
        });
        i = body_end;
    }
    posts
}

/// Same header detection as `watch::is_header_line` — kept local so
/// the parser doesn't take a public dependency on watch internals.
/// If watch's contract ever changes, mirror it here.
fn is_header_line(line: &str) -> bool {
    if !line.starts_with('[') || !line.contains("] ") {
        return false;
    }
    if line.starts_with("[<") {
        return false;
    }
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

fn parse_header(line: &str) -> (String, String, String) {
    let bytes = line.as_bytes();
    let ts_start = bytes.len().saturating_sub(20);
    let timestamp = if bytes.len() >= 20 {
        std::str::from_utf8(&bytes[ts_start..])
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    };
    let sender = if line.starts_with('[') {
        line.find(']')
            .map(|close| line[1..close].to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let after_sender_start = line.find("] ").map(|i| i + 2).unwrap_or(0);
    let subject_slice = if ts_start > after_sender_start {
        &line[after_sender_start..ts_start]
    } else {
        ""
    };
    let subject = subject_slice
        .trim()
        .trim_end_matches('—')
        .trim()
        .to_string();
    (sender, subject, timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
=== \n\
[design] online — 2026-05-28T14:30:00Z\n\
===\n\
\n\
design session started. Standing by.\n\
\n\
(Informational, no response required.)\n\
===\n\
\n\
\n\
===\n\
[giga] [giga 2026-06-05 22:21 PST] ack — 2026-06-06T05:19:36Z\n\
===\n\
\n\
Confirmed your diagnosis.\n\
Multi-line body works fine.\n\
\n\
WAITING ON: design (review pass).\n\
===\n";

    #[test]
    fn parse_extracts_both_posts() {
        let posts = parse(SAMPLE);
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].sender, "design");
        assert_eq!(posts[0].timestamp_iso, "2026-05-28T14:30:00Z");
        assert_eq!(posts[1].sender, "giga");
        assert_eq!(posts[1].timestamp_iso, "2026-06-06T05:19:36Z");
    }

    #[test]
    fn parse_strips_trailing_em_dash_from_subject() {
        let posts = parse(SAMPLE);
        assert_eq!(posts[0].subject, "online");
        assert!(posts[1].subject.starts_with("[giga 2026-06-05"));
        assert!(
            !posts[1].subject.ends_with('—'),
            "subject should not end with em-dash: {}",
            posts[1].subject
        );
    }

    #[test]
    fn parse_preserves_body_lines_including_blanks() {
        let posts = parse(SAMPLE);
        assert!(posts[1].body.contains("Confirmed your diagnosis."));
        assert!(posts[1].body.contains("Multi-line body works fine."));
        assert!(posts[1].body.contains("WAITING ON: design"));
    }

    #[test]
    fn parse_returns_empty_for_empty_file() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn parse_ignores_preamble_text_before_first_post() {
        let with_preamble =
            format!("Convention placeholder text: [<sender>] foo — bar\n\n{SAMPLE}");
        let posts = parse(&with_preamble);
        // Preamble doesn't form a valid header (starts with [<), so it's skipped.
        assert_eq!(posts.len(), 2);
    }

    #[test]
    fn is_header_line_accepts_real_header() {
        assert!(is_header_line("[design] online — 2026-05-28T14:30:00Z"));
    }

    #[test]
    fn is_header_line_rejects_inner_address() {
        assert!(!is_header_line("[web] — explicit GO for the new feature"));
    }

    #[test]
    fn is_header_line_handles_multibyte_tail_without_panic() {
        // Em-dash near (but not exactly at) the 20-byte tail boundary.
        // Used to panic before watch.rs adopted byte-slice checking.
        assert!(!is_header_line(
            "[alice] — relocate the FULL stack (NOT a feature — fits the freeze)."
        ));
    }

    #[test]
    fn parse_header_extracts_sender_subject_and_ts() {
        let (sender, subject, ts) = parse_header(
            "[design] [design 2026-06-05 13:50 PST] giga-rearm execve breaks watchers — 2026-06-05T20:43:01Z",
        );
        assert_eq!(sender, "design");
        assert!(subject.starts_with("[design 2026-06-05"));
        assert!(subject.contains("giga-rearm"));
        assert_eq!(ts, "2026-06-05T20:43:01Z");
    }
}
