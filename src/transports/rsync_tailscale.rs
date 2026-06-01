//! v0.2's default — rsync over Tailscale SSH. This is a thin adapter:
//! the actual rsync planning + execution lives in `crate::sync`
//! (which the v0.2 release shipped, unchanged); the SSH passthrough
//! for `giga remote --host` lives in `crate::remote`.
//!
//! Stage 1 of the v0.3.0 plug refactor keeps those modules' bodies
//! where they are (well-tested + working in prod) and just wraps them
//! in the `Transport` trait. Later stages may inline the logic into
//! this module if cleanliness wins out over diff-size.

use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::transport::Transport;

pub struct RsyncTailscaleTransport;

impl Transport for RsyncTailscaleTransport {
    fn name(&self) -> &'static str {
        "rsync+tailscale"
    }

    fn tick(&self, cfg: &Config, this_host: &str) -> Result<()> {
        crate::sync::tick_once(cfg, this_host, /* dry_run */ false)
    }

    fn bootstrap_peer(&self, cfg: &Config, peer: &str, config_path: &Path) -> Result<()> {
        crate::sync::bootstrap_peer(cfg, peer, config_path)
    }

    fn supports_remote_exec(&self) -> bool {
        true
    }

    fn run_remote(&self, cfg: &Config, peer: &str, args: &[String]) -> Result<i32> {
        crate::remote::run_passthrough(cfg, peer, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_stable_identifier() {
        assert_eq!(RsyncTailscaleTransport.name(), "rsync+tailscale");
    }

    #[test]
    fn supports_remote_exec_true() {
        assert!(RsyncTailscaleTransport.supports_remote_exec());
    }
}
