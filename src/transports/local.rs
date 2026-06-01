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
use crate::transport::Transport;

pub struct LocalTransport;

impl Transport for LocalTransport {
    fn name(&self) -> &'static str {
        "local"
    }

    fn tick(&self, _cfg: &Config, _this_host: &str) -> Result<()> {
        Ok(())
    }

    fn bootstrap_peer(&self, _cfg: &Config, peer: &str, _config_path: &Path) -> Result<()> {
        Err(anyhow!(
            "local transport can't bootstrap peer `{peer}` — this swarm is single-host. \
             Add `[transport]` with kind = \"rsync+tailscale\" or \"git\" + register peers \
             via `giga add-host` to go multi-host."
        ))
    }

    fn supports_remote_exec(&self) -> bool {
        false
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
        assert!(t.tick(&empty_cfg(), "this").is_ok());
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
    fn supports_remote_exec_false() {
        assert!(!LocalTransport.supports_remote_exec());
    }

    #[test]
    fn run_remote_errors_with_default_message() {
        let t = LocalTransport;
        let err = t.run_remote(&empty_cfg(), "wsl-b", &["sweep".into()]).unwrap_err();
        assert!(err.to_string().contains("--host commands not supported"));
    }
}
