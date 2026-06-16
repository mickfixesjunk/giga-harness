//! TOML config schema for giga-harness.
//!
//! A config describes a project's agent ecosystem: which agents
//! exist, where they work, which channels they participate in,
//! and how the bench-coordination protocol is scoped (single host
//! vs. multi-host).
//!
//! Remote-channels extension (per REMOTE_DESIGN.md):
//! - `[[hosts]]` table enumerates every host in the swarm.
//! - `[[agents]].host` names which host an agent runs on.
//! - `this_host` (the host identity of THIS machine) is loaded from a
//!   sibling `this_host.toml` next to the canonical config so rsync of
//!   the canonical doesn't trample per-host identity.
//!
//! All three additions are backward-compatible: a config with no
//! `[[hosts]]` and no `this_host.toml` behaves exactly as today
//! (local-only mode).
//!
//! See `examples/minimal/giga-harness.toml` for a working example.
//!
//! This module is split into siblings:
//! - `schema`    — the `#[derive(Deserialize)]` types + their defaults.
//! - `load`      — the load/parse/path-default pipeline.
//! - `validate`  — semantic validation, decomposed per-invariant.
//! - `resolve`   — read-side resolvers (agent→host, channel→path, …).
//! - `broadcast` — broadcast message-semantics (prefix parsing, fanout).

mod broadcast;
pub mod edit;
mod load;
pub mod resolve;
mod schema;
mod validate;

// Public surface re-exported so `crate::config::X` keeps resolving
// exactly as before the split. (Methods on `Config` live in the
// sibling `impl` blocks and are reachable via the re-exported type.)
pub use broadcast::BroadcastPrefix;
pub use broadcast::{fanout_delay_seconds, is_broadcast_channel, parse_broadcast_prefix};
// Re-exported so `config::edit_then_validate_with_rollback` resolves;
// callers currently reach it via the fully-qualified `config::edit::`
// path, hence the allow (matches the schema re-export pattern below).
#[allow(unused_imports)]
pub use edit::edit_then_validate_with_rollback;
pub use resolve::{derive_bilateral_with_platforms, DerivedChannel};
pub use schema::{Agent, Channel, Config, Host};
pub use schema::{THIS_HOST_FILE, THIS_HOST_FILE_LEGACY};
// These schema types round out the public surface but aren't currently
// referenced through `config::` qualified paths from elsewhere in the
// bin crate; the re-export keeps `crate::config::X` resolving (matching
// the pre-split `pub struct` surface) without a spurious unused-import
// warning.
#[allow(unused_imports)]
pub use schema::{
    BenchProtocol, BroadcastConfig, GitTransportConfig, Paths, Project, TransportConfig,
    WatchConfig,
};
