//! `giga sync` — push local slice files + canonical TOML to peer hosts.
//!
//! Per REMOTE_DESIGN.md §4: v1 transport is rsync over Tailscale SSH.
//! Each host pushes:
//!   - Its OWN slice files `<channel>.<this_host>.md` for every cross-host
//!     channel it participates in (single-writer-per-slice preserved at
//!     the wire level — a peer never pulls or rewrites our local data).
//!   - The canonical `giga-harness.toml` (so peers learn about config
//!     changes made from this host — operator-UX assumes one writer per
//!     swarm).
//!
//! Reception is symmetric: peers push to us; we don't pull. This means
//! no peer needs to know which slices exist on the others — each side
//! ships only what it owns.
//!
//! v1 transport is rsync over Tailscale SSH, invoked directly via
//! `Command::new("rsync")` — no abstraction layer. The planner
//! (`plan::compute_sync_plan`) is pure + returns `SyncCommand` values,
//! so it's testable without actually invoking rsync; the executor lives
//! in `rsync`, peer bootstrap in `bootstrap`, and this module owns the
//! long-running daemon loop (`run`).
//!
//! Assumption (v1): the canonical config dir + inbox dir paths are
//! symmetric across hosts (same absolute path everywhere). True for
//! homogeneous WSL/Linux setups with the same $HOME. Per-host path
//! overrides can be added later if needed.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::config::Config;

pub mod bootstrap;
pub mod plan;
pub mod rsync;

// Re-export the public surface so existing callers keep resolving the
// same `crate::transport::sync::{...}` paths after the decomposition.
pub use bootstrap::{bootstrap_peer, run_remote_giga_init};
// Public planner surface — re-exported so `crate::transport::sync::{...}`
// resolves them. No in-crate caller today (the executor reaches them via
// `super::plan`), but they're part of the module's documented API.
#[allow(unused_imports)]
pub use plan::{compute_sync_plan, SyncCommand};
pub use rsync::tick_once;
// `build_rsync_target` is reused by `teleport`; surface it at the
// `sync` module root where that caller expects it.
pub(crate) use rsync::build_rsync_target;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// v0.4.2 Bug 11 fix: how many ticks between config rereads. Matches
/// the watch + merger cadence (5 × 3s = ~15s). Without this, the
/// sync daemon iterates the channel/host list it captured at startup
/// forever — `giga add-agent` / `giga add-channel` after launch
/// silently disappear from the push set.
const RELOAD_EVERY_N_TICKS: u64 = 5;

/// v0.6.15: cap for the exponential backoff sleep when sync ticks are
/// failing consecutively (network down, peer offline, etc.). The
/// daemon sleeps `POLL_INTERVAL * 2^min(consecutive_failures-1, k)`
/// per failure with k chosen so the cap below kicks in around 4
/// failures (3s → 6s → 12s → 24s → 48s → capped at 60s). Resets to
/// POLL_INTERVAL on first successful tick. Matters when tailscale
/// goes down: pre-fix we hammered every 3s for hours.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Compute the daemon sleep duration for the next tick given how
/// many consecutive failures we've seen. Pure fn — tested in
/// isolation so the curve is easy to verify and tweak.
///
/// 0 failures → POLL_INTERVAL (3s). 1+ failures → exponential
/// backoff capped at MAX_BACKOFF (60s).
pub(crate) fn backoff_for(consecutive_failures: u32, base: Duration, cap: Duration) -> Duration {
    if consecutive_failures == 0 {
        return base;
    }
    // 2^n factor: 1 fail = 2× base, 2 fails = 4× base, ... .
    // Clamp the shift so we don't overflow on a billion-failure
    // counter run (would still saturate at `cap`, but cheap to bound).
    let shift = consecutive_failures.min(20);
    let factor = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let scaled = base.checked_mul(factor as u32).unwrap_or(cap);
    scaled.min(cap)
}

pub struct Args {
    pub config: PathBuf,
    /// Run one sync tick then exit. Useful for `giga sync --once` in
    /// scripts or for debugging.
    pub once: bool,
    /// Print the rsync commands that would be run, don't execute them.
    /// Combined with `--once` for a no-side-effects preview.
    pub dry_run: bool,
    /// v0.3.6: suppress per-tick "tick complete" summary lines. Errors
    /// and one-shot startup info still emit. Used by swarm_boss
    /// Monitor invocations.
    pub quiet: bool,
}

pub fn run(args: Args) -> Result<()> {
    let mut cfg = Config::load(&args.config)?;
    if cfg.hosts.is_empty() {
        eprintln!("sync: no [[hosts]] declared — local-only swarm, nothing to sync. Exiting.");
        return Ok(());
    }
    let this_host = cfg
        .this_host
        .clone()
        .ok_or_else(|| anyhow!("this_host is unknown — set sibling this_host.toml"))?;

    let transport = crate::transport::for_config(&cfg)?;
    // Startup line emits even in --quiet so the operator knows the
    // daemon is alive after spawn.
    eprintln!(
        "sync: transport=`{}`, this_host=`{this_host}`{}",
        transport.name(),
        if args.quiet { " (--quiet)" } else { "" },
    );

    // Transport-specific fail-fast prereq check (rsync+tailscale needs
    // `rsync` on PATH; git needs `git`). Each plug owns its own check
    // via Transport::self_check; skipped under --dry-run since no
    // external binary is invoked then.
    if !args.dry_run {
        transport.self_check()?;
    }

    let mut tick: u64 = 0;
    // v0.6.15: consecutive failure counter drives the exponential
    // backoff sleep so we don't hammer a downed tailnet every 3s.
    let mut consecutive_failures: u32 = 0;
    loop {
        let ctx = crate::transport::TickCtx {
            cfg: &cfg,
            this_host: &this_host,
            dry_run: args.dry_run,
            quiet: args.quiet,
        };
        match transport.tick(&ctx) {
            Ok(()) => {
                if consecutive_failures > 0 {
                    // Errors always emit even under --quiet — recovery
                    // from a backoff window is operator-relevant.
                    eprintln!(
                        "sync: recovered after {consecutive_failures} consecutive failed tick(s) — back to normal cadence"
                    );
                }
                consecutive_failures = 0;
            }
            Err(e) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                eprintln!(
                    "sync: {} tick failed ({e}) — will retry next tick (consecutive failures: {})",
                    transport.name(),
                    consecutive_failures,
                );
            }
        }
        if args.once {
            return Ok(());
        }
        let sleep_for = backoff_for(consecutive_failures, POLL_INTERVAL, MAX_BACKOFF);
        if consecutive_failures >= 3 && sleep_for > POLL_INTERVAL {
            // Surface the backoff once it's actually slowing things
            // down — gives the operator a visible signal that the
            // daemon noticed the persistent failure and is throttling.
            eprintln!(
                "sync: backing off (failures={consecutive_failures}, next try in {}s)",
                sleep_for.as_secs(),
            );
        }
        thread::sleep(sleep_for);
        tick = tick.wrapping_add(1);
        // v0.4.2 Bug 11 fix: re-read the config every RELOAD_EVERY_N_TICKS.
        // Without this, `giga add-agent` / `giga add-channel` after the
        // daemon launches are silently invisible — the push set is the
        // startup snapshot. Reload failure (transient TOML race) keeps
        // the current cfg, matching watch + merger behavior.
        if tick.is_multiple_of(RELOAD_EVERY_N_TICKS) {
            match Config::load(&args.config) {
                Ok(new_cfg) => {
                    cfg = new_cfg;
                }
                Err(e) => {
                    eprintln!("sync: config reload failed ({e}) — keeping previous snapshot");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0.6.15 regression guard. backoff_for is a pure function;
    /// these tests pin the curve so future tuning is intentional.
    #[test]
    fn backoff_for_zero_failures_returns_base() {
        let base = Duration::from_secs(3);
        let cap = Duration::from_secs(60);
        assert_eq!(backoff_for(0, base, cap), base);
    }

    #[test]
    fn backoff_for_grows_exponentially_until_cap() {
        let base = Duration::from_secs(3);
        let cap = Duration::from_secs(60);
        // 1 fail → 2× base = 6s
        assert_eq!(backoff_for(1, base, cap), Duration::from_secs(6));
        // 2 fails → 4× = 12s
        assert_eq!(backoff_for(2, base, cap), Duration::from_secs(12));
        // 3 fails → 8× = 24s
        assert_eq!(backoff_for(3, base, cap), Duration::from_secs(24));
        // 4 fails → 16× = 48s (still under cap)
        assert_eq!(backoff_for(4, base, cap), Duration::from_secs(48));
        // 5 fails → 32× = 96s, but cap kicks in
        assert_eq!(backoff_for(5, base, cap), cap);
        // Many fails → still cap, never overflow.
        assert_eq!(backoff_for(50, base, cap), cap);
        assert_eq!(backoff_for(u32::MAX, base, cap), cap);
    }
}
