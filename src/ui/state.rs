//! Per-server shared state. Cheap to `.clone()` (everything is
//! Arc-backed).
//!
//! v0.6.35 Phase D introduces `tailers` — a per-`(swarm, channel-file)`
//! `broadcast::Sender<Post>` that fan-outs newly-appended posts to all
//! WebSocket subscribers of that channel. Each entry has a backing
//! tokio task (the "tailer") that polls the file's contents and
//! broadcasts deltas. Multiple subscribers share one tailer per
//! channel — work-deduplicated.

use crate::ui::channel::Post;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

#[derive(Clone, Default)]
pub struct AppState {
    pub tailers: Arc<RwLock<HashMap<(String, String), broadcast::Sender<Post>>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }
}
