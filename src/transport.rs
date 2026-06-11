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

/// Pluggable swarm-state transport. See module docs.
pub trait Transport: Send + Sync {
    /// Short stable identifier for logs + error messages. Matches the
    /// `[transport.kind]` TOML value (e.g. "git", "rsync+tailscale").
    fn name(&self) -> &'static str;

    // ----- Slice-and-merge sync (mandatory) -----

    /// Long-running daemon's per-tick work. Push own slices + canonical
    /// TOML to wherever peers can pick them up; pull peer slices into
    /// local inbox. Idempotent. Daemon retries on next tick if Err.
    ///
    /// `dry_run = true` should print the plan to stderr without making
    /// any persistent changes (used by `giga sync --once --dry-run` for
    /// operator debugging). Plugs MAY ignore the flag if their work is
    /// hard to enumerate without doing it.
    fn tick(&self, cfg: &Config, this_host: &str, dry_run: bool) -> Result<()>;

    /// One-shot peer bootstrap. Called by `giga add-host` and
    /// `giga add-agent --host` after the local TOML edit. Should leave
    /// the peer in a state where its own sync daemon can pick up the
    /// swarm + start ticking.
    ///
    /// Best-effort: callers warn on failure rather than blocking local
    /// success (peer may be offline; sync recovers later).
    fn bootstrap_peer(&self, cfg: &Config, peer: &str, config_path: &Path) -> Result<()>;

    // ----- Command-on-peer (optional capability) -----

    /// Whether this transport can run synchronous commands on a peer.
    /// `giga remote --host`, `giga sweep --host`, `giga launch --host`
    /// require this. Returns false → those flags error cleanly.
    fn supports_remote_exec(&self) -> bool {
        false
    }

    /// Run a giga subcommand on a peer. Default impl errors with a
    /// clear "this transport doesn't support --host commands" message.
    /// Plugs that return true from `supports_remote_exec` MUST override.
    fn run_remote(&self, _cfg: &Config, _peer: &str, _args: &[String]) -> Result<i32> {
        Err(anyhow!(
            "{}: --host commands not supported by this transport. \
             Run giga commands locally on the peer instead.",
            self.name()
        ))
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
        "local" => Ok(Box::new(crate::transports::local::LocalTransport)),
        "rsync+tailscale" => Ok(Box::new(
            crate::transports::rsync_tailscale::RsyncTailscaleTransport,
        )),
        "git" => Ok(Box::new(crate::transports::git::GitTransport::from_config(
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
    fn supports_remote_exec_default_is_false() {
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
        assert!(!t.supports_remote_exec());
    }
}
