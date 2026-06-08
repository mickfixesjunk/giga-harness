//! `giga ui` — browser-based dashboard for managing every swarm
//! registered on this machine.
//!
//! See `workdirs/giga/UI_DESIGN.md` for the full design + plan.
//!
//! v0.6.31 Phase A scope: skeleton only.
//! - Binds an axum server on `127.0.0.1:<port>`.
//! - Serves a "hello world" placeholder at `/`.
//! - Health endpoint at `/api/health` returns the running version.
//! - Single-instance enforcement via a PID file at `~/.giga/ui.pid`.
//! - Graceful shutdown on Ctrl-C; removes the PID file on exit.
//!
//! Later phases bolt on the swarm/channel/process APIs, the
//! WebSocket tail, and the embedded Svelte frontend.

use anyhow::Result;
use std::path::PathBuf;

pub mod api;
pub mod channel;
pub mod pid;
pub mod process;
pub mod server;
pub mod state;
pub mod ws;

pub struct Args {
    pub bind: String,
    pub port: u16,
}

pub fn run(args: Args) -> Result<()> {
    let pid_path = pid_file_path()?;
    let _guard = pid::acquire(&pid_path)?;

    println!("==> giga ui starting on http://{}:{}", args.bind, args.port);
    println!("    pid file: {}", pid_path.display());
    println!("    Ctrl-C to stop.");

    // Local tokio runtime — the rest of giga is synchronous CLI
    // tooling, so we keep tokio scoped to this subcommand instead
    // of leaking #[tokio::main] across the whole binary.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("building tokio runtime: {e}"))?;

    rt.block_on(server::serve(args.bind, args.port))?;

    // _guard runs Drop → unlinks the PID file on normal exit.
    Ok(())
}

/// `~/.giga/ui.pid` — where the running server stamps its PID so
/// the next `giga ui` invocation can detect "already running"
/// instead of binding a second server and getting EADDRINUSE.
fn pid_file_path() -> Result<PathBuf> {
    let home = crate::cursor::giga_home()
        .ok_or_else(|| anyhow::anyhow!("could not resolve ~/.giga (neither $HOME nor %USERPROFILE% set)"))?;
    Ok(home.join("ui.pid"))
}
