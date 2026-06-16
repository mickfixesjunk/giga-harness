//! Coordination substrate — the file-based primitives that let agents talk.
//!
//! This groups the message-passing and liveness machinery:
//!   - [`post`]         — append a properly-formatted frame to a channel
//!   - [`merger`]       — fan-in merge of per-agent inboxes
//!   - [`sweep`]        — report channel state (who owes whom)
//!   - [`stale_wait`]   — detect stale `WAITING ON` frames
//!   - [`cursor`]       — per-watcher read-position persistence
//!   - [`watch`]        — the long-running inbox watcher (delivery path)
//!   - [`codex_channel`]— codex-runtime envelope channel

pub mod codex_channel;
pub mod cursor;
pub mod merger;
pub mod post;
pub mod stale_wait;
pub mod sweep;
pub mod watch;
