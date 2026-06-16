//! Pluggable transport architecture for cross-host swarms.
//!
//! Per TRANSPORT_DESIGN.md: the slice-and-merge architecture (per-host
//! single-writer slice files + local merger appending to the watched
//! merged file) is unchanged — this layer abstracts how slices + the
//! canonical TOML ship between hosts.
//!
//! A swarm picks ONE transport for its lifetime; all hosts in the
//! swarm must use the same plug. Selection lives in `[transport.kind]`
//! in the TOML; per-kind config is under `[transport.<kind>]`.
//!
//! Three plugs ship in v0.3.0:
//!   - `local`           — single-host swarm; sync is a no-op
//!   - `rsync+tailscale` — v0.2's default, rsync over Tailscale SSH
//!   - `git`             — shared git repo as state store
//!
//! Future plugs (s3/azure/gcs/webdav) fit this same trait — no
//! interface changes needed when adding them.

use std::path::Path;

use anyhow::{anyhow, Result};

use crate::config::Config;

// Concrete transport plugs + the transport-adjacent command modules,
// all reorganized under `src/transport/` (the trait + factory live in
// this file).
pub mod git;
pub mod hosts;
pub mod local;
pub mod remote;
pub mod rsync_tailscale;
pub mod setup_remote_node;
pub mod sync;

/// Per-tick context handed to [`Transport::tick`]. Bundles the inputs a
/// plug needs for one sync sweep — replaces the old
/// `(cfg, this_host, dry_run)` parameter triple and the process-global
/// QUIET static that used to live in `sync.rs` (the `quiet` flag now
/// rides along here instead of being read from a static).
pub struct TickCtx<'a> {
    pub cfg: &'a Config,
    pub this_host: &'a str,
    pub dry_run: bool,
    pub quiet: bool,
}

/// Optional capability: run a giga subcommand synchronously on a peer.
/// Separated out of [`Transport`] so the "can this transport do remote
/// exec?" question is answered by `Transport::remote_exec()` returning
/// `Some`/`None` rather than a `supports_remote_exec()` bool paired with
/// a default-erroring `run_remote`. Only transports that genuinely
/// support `giga remote --host` (today: rsync+tailscale) implement it.
pub trait RemoteExec {
    fn run_remote(&self, cfg: &Config, peer: &str, args: &[String]) -> Result<i32>;
}

/// Pluggable swarm-state transport. See module docs.
pub trait Transport: Send + Sync {
    /// Short stable identifier for logs + error messages. Matches the
    /// `[transport.kind]` TOML value (e.g. "git", "rsync+tailscale").
    fn name(&self) -> &'static str;

    /// Fail-fast prerequisite check, run once before the daemon's tick
    /// loop starts (skipped under `--dry-run`). Plugs that depend on an
    /// external binary (rsync, git) override this to verify it's on PATH
    /// and return a clear install hint if not. Default: no prereqs.
    fn self_check(&self) -> Result<()> {
        Ok(())
    }

    // ----- Slice-and-merge sync (mandatory) -----

    /// Long-running daemon's per-tick work. Push own slices + canonical
    /// TOML to wherever peers can pick them up; pull peer slices into
    /// local inbox. Idempotent. Daemon retries on next tick if Err.
    ///
    /// `ctx.dry_run = true` should print the plan to stderr without
    /// making any persistent changes (used by `giga sync --once
    /// --dry-run` for operator debugging). Plugs MAY ignore the flag if
    /// their work is hard to enumerate without doing it.
    fn tick(&self, ctx: &TickCtx) -> Result<()>;

    /// One-shot peer bootstrap. Called by `giga add-host` and
    /// `giga add-agent --host` after the local TOML edit. Should leave
    /// the peer in a state where its own sync daemon can pick up the
    /// swarm + start ticking.
    ///
    /// Best-effort: callers warn on failure rather than blocking local
    /// success (peer may be offline; sync recovers later).
    fn bootstrap_peer(&self, cfg: &Config, peer: &str, config_path: &Path) -> Result<()>;

    // ----- Command-on-peer (optional capability) -----

    /// Return this transport's [`RemoteExec`] handle if it supports
    /// running synchronous commands on a peer (`giga remote --host`,
    /// `giga sweep --host`, `giga launch --host`). Default: `None` →
    /// those flags error cleanly. Plugs that support remote exec return
    /// `Some(self)`.
    fn remote_exec(&self) -> Option<&dyn RemoteExec> {
        None
    }
}

/// Build the right transport for this swarm.
///
/// Selection rules:
///   * Explicit `cfg.transport.kind` → use that plug.
///   * No `[transport]` section, non-empty `[[hosts]]` → infer
///     `"rsync+tailscale"` (v0.2 backward-compat).
///   * No `[transport]` section, empty `[[hosts]]` → `"local"` (legacy
///     single-host swarms).
///   * Unknown kind → Err.
pub fn for_config(cfg: &Config) -> Result<Box<dyn Transport>> {
    let kind = cfg
        .transport
        .as_ref()
        .map(|t| t.kind.as_str())
        .unwrap_or_else(|| {
            if cfg.hosts.is_empty() {
                "local"
            } else {
                "rsync+tailscale"
            }
        });

    match kind {
        "local" => Ok(Box::new(crate::transport::local::LocalTransport)),
        "rsync+tailscale" => Ok(Box::new(
            crate::transport::rsync_tailscale::RsyncTailscaleTransport,
        )),
        "git" => Ok(Box::new(crate::transport::git::GitTransport::from_config(
            cfg,
        )?)),
        other => Err(anyhow!(
            "unknown transport `{other}` — supported: local, rsync+tailscale, git"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_text(text: &str) -> Config {
        let mut cfg: Config = toml::from_str(text).unwrap();
        if !cfg.hosts.is_empty() && cfg.this_host.is_none() {
            // for validate() — tests just need a parsed Config
            cfg.this_host = Some(cfg.hosts[0].name.clone());
        }
        cfg
    }

    #[test]
    fn no_transport_section_no_hosts_infers_local() {
        let cfg = cfg_with_text(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
"#,
        );
        let t = for_config(&cfg).unwrap();
        assert_eq!(t.name(), "local");
    }

    #[test]
    fn no_transport_section_with_hosts_infers_rsync_tailscale() {
        let cfg = cfg_with_text(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "a"
tailnet_hostname = "a.tail.ts.net"
"#,
        );
        let t = for_config(&cfg).unwrap();
        assert_eq!(t.name(), "rsync+tailscale");
    }

    #[test]
    fn explicit_transport_kind_local() {
        let cfg = cfg_with_text(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[transport]
kind = "local"
"#,
        );
        let t = for_config(&cfg).unwrap();
        assert_eq!(t.name(), "local");
    }

    #[test]
    fn explicit_transport_kind_git_requires_state_repo() {
        let cfg = cfg_with_text(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[transport]
kind = "git"
[transport.git]
state_repo = "git@github.com:mick/x.git"
"#,
        );
        let t = for_config(&cfg).unwrap();
        assert_eq!(t.name(), "git");
    }

    #[test]
    fn unknown_transport_kind_errors() {
        let cfg = cfg_with_text(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[transport]
kind = "carrier-pigeon"
"#,
        );
        // Box<dyn Transport> doesn't implement Debug, so we can't use
        // unwrap_err — match instead.
        let err = match for_config(&cfg) {
            Ok(_) => panic!("expected unknown-transport error"),
            Err(e) => e,
        };
        assert!(err
            .to_string()
            .contains("unknown transport `carrier-pigeon`"));
    }

    #[test]
    fn remote_exec_default_is_none() {
        let cfg = cfg_with_text(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[transport]
kind = "local"
"#,
        );
        let t = for_config(&cfg).unwrap();
        assert!(t.remote_exec().is_none());
    }
}
