//! Channel-file post parsing for the dashboard.
//!
//! The grammar now lives in [`crate::foundation::frame`] — the one parser
//! shared by the watcher, sweep, stale-wait, and the codex bridge. This
//! module is a thin re-export so the `giga ui` call sites
//! (`post_parser::parse` / `post_parser::Post`) keep their local name.

pub use crate::foundation::frame::{parse_posts as parse, Post};
