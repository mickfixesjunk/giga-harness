//! Shared post-mutation peer bootstrap, used by `add-agent --host` and
//! `add-host`.
//!
//! After a host/agent add that touches a non-local peer, the canonical
//! TOML needs to reach the peer (so it learns about the change) and —
//! for an agent add — the peer needs a `giga init` to scaffold the new
//! agent's workdir. Both steps are BEST-EFFORT: on failure we warn but
//! never fail the local-side success, because the local config is
//! already correct and `giga sync --once` can recover the peer later.
//!
//! Previously add-agent and add-host each hand-rolled this push +
//! warn-don't-fail dance with slightly different prose; this module is
//! the single home for it.

use std::path::Path;

use crate::config::Config;
use crate::transport::sync;

/// Push the canonical TOML to `peer` (mkdir + rsync + ensure
/// this_host.toml), and — when `run_remote_init` is set — run a remote
/// `giga init` so the peer scaffolds any new agent workdirs.
///
/// Best-effort: prints progress + warnings, never returns an error. The
/// caller has already committed the local change; the peer is just
/// downstream of it.
pub fn bootstrap_peer_best_effort(
    cfg: &Config,
    peer: &str,
    config_path: &Path,
    run_remote_init: bool,
) {
    println!("auto-bootstrap: pushing canonical TOML to `{peer}`...");
    let bootstrap_ok = match sync::bootstrap_peer(cfg, peer, config_path) {
        Ok(()) => {
            println!("  + canonical TOML synced to `{peer}` (and this_host.toml ensured)");
            true
        }
        Err(e) => {
            eprintln!("  ! auto-bootstrap failed: {e:#}");
            eprintln!("    The local config is correct; the peer just isn't synced yet.");
            eprintln!("    Run `giga sync --once` once everything is reachable to recover.");
            false
        }
    };

    // Remote `giga init` scaffolds the new agent's workdir + AGENTS.md on
    // the peer. Init is host-aware, so it only touches workdirs for
    // agents whose `host` matches the peer. Only runs if bootstrap
    // succeeded (otherwise the peer doesn't even have the TOML to init
    // from).
    if run_remote_init && bootstrap_ok {
        println!("auto-scaffold: running `giga init` on `{peer}`...");
        match sync::run_remote_giga_init(cfg, peer, config_path) {
            Ok(()) => {
                println!(
                    "  + remote init complete — peer's workdir(s) + AGENTS.md ready on `{peer}`"
                )
            }
            Err(e) => {
                eprintln!("  ! remote giga init failed: {e:#}");
                eprintln!("    The peer has the TOML; run `giga remote --host {peer} init` manually to scaffold.");
            }
        }
    }
}
