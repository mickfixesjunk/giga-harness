//! Scaffolding commands: turn a swarm config into on-disk artifacts and
//! running terminals.
//!
//! - [`init`] writes inbox files + per-agent AGENTS.md (effects: mkdir,
//!   file writes, symlink, trust, registry upsert).
//! - [`render`] holds the pure AGENTS.md / channel-header text generation
//!   that `init` (and `takeover`) call — no filesystem side effects.
//! - [`launch`] builds the per-agent pane plan and hands it to a terminal
//!   backend.
//! - [`terminal`] is the cross-platform terminal multiplexer abstraction
//!   ([`terminal::TerminalBackend`]) plus per-backend implementations.
//! - [`templates`] exposes the compiled-in AGENTS.md template strings.

pub mod init;
pub mod launch;
pub mod render;
pub mod templates;
pub mod terminal;
