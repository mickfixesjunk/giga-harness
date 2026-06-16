//! The one canonical timestamp format used in channel-frame headers.
//!
//! Every `===`-frame header ends with a UTC ISO-8601 timestamp in
//! exactly this shape — 20 ASCII bytes ending in `Z`, e.g.
//! `2026-05-22T10:14:00Z`. The 20-byte invariant is load-bearing: the
//! watcher's `is_header_line` check keys off it (see
//! [`crate::foundation::frame`]). Keeping the format literal in one place
//! stops the producers (`post`, takeover/teleport banners) and the
//! consumers (`watch`, `sweep`, `stale_wait`) from drifting apart.

use chrono::{DateTime, NaiveDateTime, Utc};

/// `strftime` format for frame-header timestamps. 20 bytes when rendered
/// (`YYYY-MM-DDTHH:MM:SSZ`).
pub const GIGA_TS_FMT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// The current instant rendered in [`GIGA_TS_FMT`].
pub fn now_iso8601() -> String {
    Utc::now().format(GIGA_TS_FMT).to_string()
}

/// Parse a frame-header timestamp string back into a `DateTime<Utc>`.
/// Returns `None` for anything not in [`GIGA_TS_FMT`]. Does not enforce
/// the 20-byte length itself — callers that need the strict header check
/// use [`crate::foundation::frame`].
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, GIGA_TS_FMT)
        .ok()
        .map(|naive| naive.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_twenty_bytes_ending_in_z() {
        let s = now_iso8601();
        assert_eq!(s.len(), 20, "got {s:?}");
        assert!(s.ends_with('Z'));
    }

    #[test]
    fn round_trips_through_parse() {
        let s = "2026-06-05T00:00:00Z";
        let dt = parse_ts(s).unwrap();
        assert_eq!(dt.format(GIGA_TS_FMT).to_string(), s);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_ts("not a timestamp").is_none());
        assert!(parse_ts("2026-06-05").is_none());
        assert!(parse_ts("2026-06-05T00:00:00").is_none()); // missing Z
    }
}
