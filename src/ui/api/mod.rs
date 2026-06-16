//! REST handlers for the dashboard.
//!
//! Phase B (v0.6.32):
//!   * `GET /api/swarms` — list every registered swarm with a
//!     summary (agent count, channel count, last activity).
//!   * `GET /api/swarms/:name` — full detail (agents + channels).
//!
//! Stateless: each request reloads `~/.giga/swarms.toml` and the
//! per-swarm `giga-harness.toml`. Caching is a future optimization.
//!
//! Split (Phase 12) into:
//!   * [`read`] — the GET handlers + their DTOs.
//!   * [`mutate`] — the POST handlers (shell out via `run_giga`,
//!     tmux, or the post machinery).
//!   * [`dto`] — the shared error/exec primitives.
//!
//! The router in `ui/server.rs` references handlers as `api::<fn>`,
//! so every handler is re-exported here verbatim.

mod dto;
mod mutate;
mod read;

// Read (GET) handlers — referenced by the router as `api::<fn>`.
pub use read::{
    get_agent_log, get_channel_tail, get_swarm, get_swarm_timeline, list_processes, list_swarms,
};

// Mutate (POST) handlers — referenced by the router as `api::<fn>`.
pub use mutate::{
    add_agent, add_channel, kill_swarm, launch_swarm, post_to_channel, run_upgrade,
    set_swarm_archived, validate_swarm,
};

// DTOs + shared error/exec primitives. Re-exported so the historical
// `crate::ui::api::<Type>` paths keep resolving; `allow(unused_imports)`
// because these are part of the public surface but not all are consumed
// outside their defining submodule.
#[allow(unused_imports)]
pub use dto::{ExecResult, PostError};
#[allow(unused_imports)]
pub use mutate::{
    AddAgentBody, AddChannelBody, ArchiveBody, ArchiveResult, LaunchQuery, PostBody, PostResponse,
    UpgradeQuery,
};
#[allow(unused_imports)]
pub use read::{
    AgentDto, AgentProcessStatus, ChannelDto, ChannelTail, LogQuery, LogSnapshot, SwarmDetail,
    SwarmSummary, TailQuery, Timeline, TimelinePost,
};
