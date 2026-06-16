//! Config-mutating subcommands.
//!
//! Each of these commands edits the canonical `giga-harness.toml`
//! in-place (preserving comments + formatting via `toml_edit`) and
//! routes the write through
//! [`crate::config::edit::edit_then_validate_with_rollback`], so a post-
//! edit validation failure never leaves a half-committed config on disk.
//!
//! - [`add_agent`]      — scaffold a new agent (+ peer channels + template)
//! - [`add_channel`]    — append a bilateral channel between two agents
//! - [`add_host`]       — register a `[[hosts]]` entry (+ first-host migration)
//! - [`set_swarm_boss`] — promote/demote an agent's `swarm_boss` flag
//!
//! [`peer_bootstrap`] holds the shared best-effort "push the canonical
//! TOML to a peer (+ optionally remote `giga init`), warn-don't-fail"
//! logic that add-agent (`--host`) and add-host both use after their
//! edit lands.

pub mod add_agent;
pub mod add_channel;
pub mod add_host;
pub mod peer_bootstrap;
pub mod set_swarm_boss;
