//! v0.2's default — rsync over Tailscale SSH. This is a thin adapter:
//! the actual rsync planning + execution lives in `crate::transport::sync`
//! (which the v0.2 release shipped, unchanged); the SSH passthrough
//! for `giga remote --host` lives in `crate::transport::remote`.
//!
//! Stage 1 of the v0.3.0 plug refactor keeps those modules' bodies
//! where they are (well-tested + working in prod) and just wraps them
//! in the `Transport` trait. Later stages may inline the logic into
//! this module if cleanliness wins out over diff-size.

use std::path::Path;

use anyhow::{anyhow, Result};

use crate::config::Config;
use crate::transport::{RemoteExec, TickCtx, Transport};

pub struct RsyncTailscaleTransport;

impl Transport for RsyncTailscaleTransport {
    fn name(&self) -> &'static str {
        "rsync+tailscale"
    }

    fn self_check(&self) -> Result<()> {
        if which::which("rsync").is_err() {
            return Err(anyhow!(
                "rsync not found on PATH. Install it with: sudo apt install rsync"
            ));
        }
        Ok(())
    }

    fn tick(&self, ctx: &TickCtx) -> Result<()> {
        crate::transport::sync::tick_once(ctx.cfg, ctx.this_host, ctx.dry_run, ctx.quiet)
    }

    fn bootstrap_peer(&self, cfg: &Config, peer: &str, config_path: &Path) -> Result<()> {
        crate::transport::sync::bootstrap_peer(cfg, peer, config_path)
    }

    fn remote_exec(&self) -> Option<&dyn RemoteExec> {
        Some(self)
    }
}

impl RemoteExec for RsyncTailscaleTransport {
    fn run_remote(&self, cfg: &Config, peer: &str, args: &[String]) -> Result<i32> {
        crate::transport::remote::run_passthrough(cfg, peer, args)
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
    fn remote_exec_is_some() {
        assert!(RsyncTailscaleTransport.remote_exec().is_some());
    }
}
