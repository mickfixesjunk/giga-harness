//! The canonical `===`-delimited channel-frame grammar.
//!
//! Every channel post is a frame:
//!
//! ```text
//! ===
//! [<sender>] <subject> — <UTC-ISO-8601-timestamp>
//! ===
//!
//! <body lines>
//!
//! WAITING ON: <agent> (<needs>)        |  (Informational, no response required.)
//! ===
//! ```
//!
//! Five modules used to each hand-roll a header parser, and they had
//! quietly diverged — three different header-detection rules, two subject
//! extractors, and one (`codex_channel`) that still byte-sliced a `&str`
//! and could panic on a multibyte char straddling the 20-byte timestamp
//! tail. This is the one grammar they all share now.
//!
//! ## Header detection vs. extraction
//!
//! [`is_header_line`] is the cheap structural gate the watcher runs on
//! every line (its self-filter is what stops echo notifications). It only
//! checks shape: `[sender] … <20-byte-ts>`. [`parse_header`] is the
//! precise extractor — it additionally validates the timestamp is a real
//! date. Contract: `parse_header(l).is_some()` implies `is_header_line(l)`.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::timefmt;

/// A parsed frame header: `[sender] subject — ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub sender: String,
    pub subject: String,
    pub ts: DateTime<Utc>,
}

impl Header {
    /// The timestamp rendered back into the canonical 20-byte form.
    pub fn ts_iso(&self) -> String {
        self.ts.format(timefmt::GIGA_TS_FMT).to_string()
    }
}

/// A frame footer — who, if anyone, owes the next move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Footer {
    /// `WAITING ON: <agent>` — a reply is owed by that agent.
    WaitingOn(String),
    /// `(Informational, no response required.)` or a WAITING-ON whose
    /// target is a synonym for "nobody".
    Informational,
}

/// Whether `line` is a frame header. Cheap, allocation-free, panic-safe:
///
/// - starts with `[` and contains `] `,
/// - is not the `[<sender>]` convention-preamble placeholder,
/// - ends with a 20-byte UTC timestamp tail (`…NN-NN-NNTNN:NN:NNZ`).
///
/// The tail is checked over the line's raw bytes (never a `&str` slice)
/// so an em-dash near the boundary can't cause a panic.
pub fn is_header_line(line: &str) -> bool {
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

/// Parse a header line into its [`Header`] parts, or `None` if it isn't a
/// well-formed header. The sender is the first `[...]` group; the subject
/// is everything between `] ` and the trailing ` — <ts>` (em-dash split
/// from the right, so an em-dash inside the subject is preserved); the
/// timestamp must parse as a real UTC date.
pub fn parse_header(line: &str) -> Option<Header> {
    if !is_header_line(line) {
        return None;
    }
    let bytes = line.as_bytes();
    let ts_start = bytes.len() - 20;
    // is_header_line proved the structure, but the 14 unchecked tail
    // bytes could (pathologically) be a multibyte continuation — read the
    // tail via from_utf8 so a non-boundary yields None instead of a panic.
    let ts_raw = std::str::from_utf8(&bytes[ts_start..]).ok()?;
    let ts = timefmt::parse_ts(ts_raw)?;

    let bracket_end = line.find("] ")?;
    let sender = line[1..bracket_end].to_string();
    if sender.is_empty() {
        return None;
    }
    let after = bracket_end + 2;
    let subject = match line.get(after..ts_start) {
        Some(s) => s.trim().trim_end_matches('—').trim().to_string(),
        None => String::new(),
    };
    Some(Header {
        sender,
        subject,
        ts,
    })
}

/// Parse a footer line. `WAITING ON: <who>` yields [`Footer::WaitingOn`]
/// unless `who` is a synonym for nobody (`none`, `nobody`, `n/a`, …), in
/// which case it's [`Footer::Informational`]; an
/// `(Informational, no response required.)` line is also Informational.
/// Returns `None` for non-footer lines. Leading whitespace is tolerated.
pub fn parse_footer(line: &str) -> Option<Footer> {
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
        let lower = who.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "nobody" | "none" | "no-one" | "noone" | "n/a" | "informational"
        ) {
            return Some(Footer::Informational);
        }
        return Some(Footer::WaitingOn(who.to_string()));
    }
    if trimmed.contains("Informational, no response required") {
        return Some(Footer::Informational);
    }
    None
}

/// The last frame in a channel body: its header plus the footer that
/// follows it (if any, before the next header). This is what `giga sweep`
/// reduces a channel to — "who sent the most recent message, about what,
/// and who owes the reply".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFrame {
    pub header: Header,
    pub footer: Option<Footer>,
}

impl LastFrame {
    /// The agent this frame is waiting on, if any.
    pub fn waiting_on(&self) -> Option<&str> {
        match &self.footer {
            Some(Footer::WaitingOn(who)) => Some(who),
            _ => None,
        }
    }
}

/// Find the last header in `body` and the footer that closes its frame.
pub fn last_header_block(body: &str) -> Option<LastFrame> {
    let lines: Vec<&str> = body.lines().collect();
    let idx = lines.iter().rposition(|l| is_header_line(l))?;
    let header = parse_header(lines[idx])?;
    let mut footer = None;
    for line in lines.iter().skip(idx + 1) {
        if is_header_line(line) {
            break; // next frame started without a footer
        }
        if let Some(f) = parse_footer(line) {
            footer = Some(f);
            break;
        }
    }
    Some(LastFrame { header, footer })
}

/// A fully-extracted post for display (the `giga ui` channel-tail DTO):
/// header parts plus the body text between the header's closing `===` and
/// the post's closing `===`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Post {
    pub sender: String,
    pub subject: String,
    /// The 20-byte UTC timestamp tail, e.g. `2026-06-05T20:43:01Z`.
    pub timestamp_iso: String,
    pub body: String,
}

/// Parse every post in a channel file's text, oldest first. Malformed
/// blocks (no valid header) are skipped. Use `.iter().rev().take(n)` for
/// the most-recent N.
pub fn parse_posts(content: &str) -> Vec<Post> {
    let lines: Vec<&str> = content.lines().collect();
    let mut posts = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != "===" {
            i += 1;
            continue;
        }
        let header_idx = i + 1;
        let body_start = i + 3;
        if body_start > lines.len() {
            break;
        }
        let header = match parse_header(lines[header_idx]) {
            Some(h) if lines[i + 2].trim() == "===" => h,
            _ => {
                i += 1;
                continue;
            }
        };
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
        let timestamp_iso = header.ts_iso();
        posts.push(Post {
            sender: header.sender,
            subject: header.subject,
            timestamp_iso,
            body,
        });
        i = body_end;
    }
    posts
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_header_line: the union of watch/codex/ui header-detection tests ---

    #[test]
    fn real_header_passes() {
        assert!(is_header_line("[design] online — 2026-05-28T14:30:00Z"));
    }

    #[test]
    fn body_line_addressing_recipient_is_rejected() {
        // the echo-bug trigger: a body line opening with [recipient] —
        assert!(!is_header_line("[web] — explicit GO for the new feature"));
        assert!(!is_header_line("[alice] — first: v0.2.29 bench results"));
    }

    #[test]
    fn preamble_placeholder_is_rejected() {
        assert!(!is_header_line("[<sender>] <subject> — <UTC...>"));
    }

    #[test]
    fn non_bracket_lines_rejected() {
        assert!(!is_header_line("just some body text"));
        assert!(!is_header_line("==="));
        assert!(!is_header_line("WAITING ON: web"));
    }

    #[test]
    fn em_dash_in_subject_still_passes() {
        assert!(is_header_line(
            "[design] bench — results — 2026-05-28T14:30:00Z"
        ));
    }

    #[test]
    fn multibyte_tail_boundary_does_not_panic() {
        // Regression for codex_channel's `line[len-20..]` panic.
        assert!(!is_header_line(
            "[alice] — relocate the FULL stack (NOT a feature — fits the freeze)."
        ));
        assert!(!is_header_line(
            "[design] aaaaaaaaaaaaaaaa — bbbbbbbbbbbbbbbb"
        ));
        assert!(is_header_line(
            "[design] bench — results — 2026-05-28T14:30:00Z"
        ));
    }

    // --- parse_header ---

    #[test]
    fn parse_header_basic() {
        let h = parse_header("[design] online — 2026-05-28T14:30:00Z").unwrap();
        assert_eq!(h.sender, "design");
        assert_eq!(h.subject, "online");
        assert_eq!(h.ts_iso(), "2026-05-28T14:30:00Z");
    }

    #[test]
    fn parse_header_keeps_em_dash_in_subject() {
        let h = parse_header("[design] bench — results — 2026-05-28T14:30:00Z").unwrap();
        assert_eq!(h.subject, "bench — results");
    }

    #[test]
    fn parse_header_keeps_inner_bracket_annotation() {
        let h =
            parse_header("[giga] [giga 2026-06-05 22:21 PST] ack — 2026-06-06T05:19:36Z").unwrap();
        assert_eq!(h.sender, "giga");
        assert_eq!(h.subject, "[giga 2026-06-05 22:21 PST] ack");
        assert_eq!(h.ts_iso(), "2026-06-06T05:19:36Z");
    }

    #[test]
    fn parse_header_rejects_invalid_date() {
        // structurally a header (byte pattern), but not a real date
        assert!(is_header_line("[x] subj — 2026-99-99T99:99:99Z"));
        assert!(parse_header("[x] subj — 2026-99-99T99:99:99Z").is_none());
    }

    #[test]
    fn parse_header_some_implies_is_header_line() {
        let l = "[design] x — 2026-05-28T14:30:00Z";
        assert!(parse_header(l).is_some());
        assert!(is_header_line(l));
    }

    // --- parse_footer ---

    #[test]
    fn footer_waiting_on_extracts_agent() {
        assert_eq!(
            parse_footer("WAITING ON: code (acknowledge + estimate)"),
            Some(Footer::WaitingOn("code".into()))
        );
    }

    #[test]
    fn footer_informational_variants() {
        assert_eq!(
            parse_footer("(Informational, no response required.)"),
            Some(Footer::Informational)
        );
        // synonyms-for-nobody collapse to Informational
        assert_eq!(
            parse_footer("WAITING ON: nobody"),
            Some(Footer::Informational)
        );
        assert_eq!(
            parse_footer("WAITING ON: none"),
            Some(Footer::Informational)
        );
        assert_eq!(parse_footer("WAITING ON: n/a"), Some(Footer::Informational));
    }

    #[test]
    fn footer_none_for_body_text() {
        assert_eq!(parse_footer("just a normal body line"), None);
        assert_eq!(parse_footer("WAITING ON: "), None); // empty target
    }

    // --- last_header_block ---

    #[test]
    fn last_header_block_picks_latest_and_its_footer() {
        let body = "\
===
[design] first — 2026-05-22T10:14:00Z
===

scope agreed.

WAITING ON: code (ack)
===


===
[code] re: first — 2026-05-22T10:31:08Z
===

acked.

(Informational, no response required.)
===
";
        let lf = last_header_block(body).unwrap();
        assert_eq!(lf.header.sender, "code");
        assert_eq!(lf.header.subject, "re: first");
        assert_eq!(lf.footer, Some(Footer::Informational));
        assert_eq!(lf.waiting_on(), None);
    }

    #[test]
    fn last_header_block_reports_open_wait() {
        let body = "\
===
[design] spec — 2026-05-22T10:14:00Z
===

body

WAITING ON: code (estimate)
===
";
        let lf = last_header_block(body).unwrap();
        assert_eq!(lf.waiting_on(), Some("code"));
    }

    #[test]
    fn last_header_block_none_for_preamble_only() {
        // A channel with only the convention preamble has no real frame.
        let body = "Convention:\n[<sender>] <subject> — <UTC...>\n";
        assert!(last_header_block(body).is_none());
    }

    // --- parse_posts (the ui channel-tail DTO) + a realistic fixture ---

    /// A synthetic-but-realistic channel file exercising every parser
    /// divergence we unified: a simple frame, an em-dash subject, an
    /// inner `[annotation]`, a multi-line body, a WAITING-ON footer, an
    /// informational-synonym footer, a body line that LOOKS like an
    /// address (echo trap), and the convention preamble placeholder.
    /// (Synthetic on purpose — real swarm channels are private.)
    const FIXTURE: &str = "\
This channel follows the giga convention:
[<sender>] <subject> — <UTC-ISO-8601>

===
[design] online — 2026-05-28T14:30:00Z
===

design session started. Standing by.

(Informational, no response required.)
===


===
[code] bench — results are in — 2026-05-28T15:02:11Z
===

Numbers look good.
[design] — heads up, see the table below
Multi-line body works fine.

WAITING ON: design (review pass)
===


===
[giga] [giga 2026-06-05 22:21 PST] ack — 2026-06-06T05:19:36Z
===

Confirmed.

WAITING ON: nobody
===
";

    #[test]
    fn parse_posts_extracts_all_frames() {
        let posts = parse_posts(FIXTURE);
        assert_eq!(posts.len(), 3, "got {posts:#?}");
        assert_eq!(posts[0].sender, "design");
        assert_eq!(posts[0].subject, "online");
        assert_eq!(posts[1].sender, "code");
        assert_eq!(posts[1].subject, "bench — results are in");
        assert_eq!(posts[2].sender, "giga");
        assert_eq!(posts[2].subject, "[giga 2026-06-05 22:21 PST] ack");
        assert_eq!(posts[2].timestamp_iso, "2026-06-06T05:19:36Z");
    }

    #[test]
    fn parse_posts_body_includes_address_like_line_not_as_header() {
        let posts = parse_posts(FIXTURE);
        // The "[design] — heads up" line is body, not a new frame.
        assert!(posts[1].body.contains("[design] — heads up"));
        assert!(posts[1].body.contains("Multi-line body works fine."));
    }

    #[test]
    fn fixture_header_count_matches_frame_count() {
        // Differential-oracle shape: # of detected headers == # of posts.
        let header_lines = FIXTURE.lines().filter(|l| is_header_line(l)).count();
        assert_eq!(header_lines, parse_posts(FIXTURE).len());
    }

    #[test]
    fn last_frame_of_fixture_is_informational_synonym() {
        let lf = last_header_block(FIXTURE).unwrap();
        assert_eq!(lf.header.sender, "giga");
        assert_eq!(lf.footer, Some(Footer::Informational));
    }
}
