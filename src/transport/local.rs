//! Local-only swarm transport — no-op everything. Active when
//! `[transport.kind] = "local"` OR (legacy v0.2 path) the config has
//! no `[[hosts]]` entries.
//!
//! The slice-and-merge fast-path (all-local channels writing direct
//! to the merged file) is implemented in `post.rs` + `merger.rs` and
//! works without any transport involvement; this plug just makes
//! `giga sync` exit cleanly + makes `bootstrap_peer` error helpfully
//! if someone misuses it.

use std::path::Path;

use anyhow::{anyhow, Result};

use crate::config::Config;
use crate::transport::{TickCtx, Transport};

pub struct LocalTransport;

impl Transport for LocalTransport {
    fn name(&self) -> &'static str {
        "local"
    }

    fn tick(&self, _ctx: &TickCtx) -> Result<()> {
        Ok(())
    }

    fn bootstrap_peer(&self, _cfg: &Config, peer: &str, _config_path: &Path) -> Result<()> {
        Err(anyhow!(
            "local transport can't bootstrap peer `{peer}` — this swarm is single-host. \
             Add `[transport]` with kind = \"rsync+tailscale\" or \"git\" + register peers \
             via `giga add-host` to go multi-host."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn empty_cfg() -> Config {
        toml::from_str(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
"#,
        )
        .unwrap()
    }

    #[test]
    fn tick_is_noop() {
        let t = LocalTransport;
        let cfg = empty_cfg();
        let ctx = TickCtx {
            cfg: &cfg,
            this_host: "this",
            dry_run: false,
            quiet: false,
        };
        assert!(t.tick(&ctx).is_ok());
        let ctx_dry = TickCtx {
            cfg: &cfg,
            this_host: "this",
            dry_run: true,
            quiet: false,
        };
        assert!(t.tick(&ctx_dry).is_ok()); // dry-run also no-op
    }

    #[test]
    fn bootstrap_peer_errors_with_helpful_message() {
        let t = LocalTransport;
        let err = t
            .bootstrap_peer(&empty_cfg(), "wsl-b", &PathBuf::from("/x.toml"))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("local transport"));
        assert!(msg.contains("single-host"));
        assert!(msg.contains("giga add-host"));
    }

    #[test]
    fn remote_exec_is_none() {
        assert!(LocalTransport.remote_exec().is_none());
    }
}
