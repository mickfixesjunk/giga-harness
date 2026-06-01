//! Git transport (stage-2 stub) — full implementation in v0.3.0 stage 2.
//!
//! Stage 1 ships a stub that:
//!   - Parses `[transport.git]` and validates `state_repo` is set
//!   - Errors on `tick()` / `bootstrap_peer()` with "git transport not
//!     yet implemented; pull stage 2" (clear signal during the refactor)
//!
//! Stage 2 fills in tick (git pull/push), bootstrap_peer (clone +
//! commit), and the per-host setup for `giga setup --remote-node
//! --transport git --repo <url>`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::config::Config;
use crate::transport::Transport;

pub struct GitTransport {
    pub state_repo: String,
    pub local_clone_dir: PathBuf,
}

impl GitTransport {
    /// Parse `[transport.git]` out of the config. Errors when the
    /// active transport is `git` but the section is missing or
    /// incomplete (no `state_repo`).
    pub fn from_config(cfg: &Config) -> Result<Self> {
        let t = cfg.transport.as_ref().ok_or_else(|| {
            anyhow!("GitTransport::from_config called without [transport] in config")
        })?;
        let git = t.git.as_ref().ok_or_else(|| {
            anyhow!(
                "transport.kind = \"git\" but no [transport.git] section — \
                 add `[transport.git]\\nstate_repo = \"git@github.com:...\"`"
            )
        })?;
        let default_clone = default_clone_dir(&cfg.project.name);
        Ok(Self {
            state_repo: git.state_repo.clone(),
            local_clone_dir: git.local_clone_dir.clone().unwrap_or(default_clone),
        })
    }
}

/// `~/.giga/swarm-state/<project>/` — the default git-transport clone
/// location when `[transport.git].local_clone_dir` isn't set.
fn default_clone_dir(project: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".giga").join("swarm-state").join(project)
}

impl Transport for GitTransport {
    fn name(&self) -> &'static str {
        "git"
    }

    fn tick(&self, _cfg: &Config, _this_host: &str) -> Result<()> {
        Err(anyhow!(
            "git transport stage-2 stub — tick() not yet implemented. \
             v0.3.0 stage 2 will land git pull/push for state_repo={}",
            self.state_repo
        ))
    }

    fn bootstrap_peer(&self, _cfg: &Config, peer: &str, _config_path: &Path) -> Result<()> {
        Err(anyhow!(
            "git transport stage-2 stub — bootstrap_peer({peer}) not yet implemented. \
             v0.3.0 stage 2 will land it."
        ))
    }

    fn supports_remote_exec(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn from_config_requires_transport_section() {
        let cfg = cfg_with(
            r#"
[project]
name = "x"
[paths]
wsl_inbox = "/tmp/i"
"#,
        );
        let err = match GitTransport::from_config(&cfg) {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("without [transport]"));
    }

    #[test]
    fn from_config_requires_git_subsection() {
        let cfg = cfg_with(
            r#"
[project]
name = "x"
[paths]
wsl_inbox = "/tmp/i"
[transport]
kind = "git"
"#,
        );
        let err = match GitTransport::from_config(&cfg) {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("[transport.git]"));
        assert!(err.to_string().contains("state_repo"));
    }

    #[test]
    fn from_config_with_state_repo_succeeds() {
        let cfg = cfg_with(
            r#"
[project]
name = "myproj"
[paths]
wsl_inbox = "/tmp/i"
[transport]
kind = "git"
[transport.git]
state_repo = "git@github.com:mick/x.git"
"#,
        );
        let t = GitTransport::from_config(&cfg).unwrap();
        assert_eq!(t.state_repo, "git@github.com:mick/x.git");
        assert!(t.local_clone_dir.ends_with("swarm-state/myproj"));
    }

    #[test]
    fn from_config_uses_local_clone_dir_override() {
        let cfg = cfg_with(
            r#"
[project]
name = "myproj"
[paths]
wsl_inbox = "/tmp/i"
[transport]
kind = "git"
[transport.git]
state_repo = "git@github.com:mick/x.git"
local_clone_dir = "/custom/clone/path"
"#,
        );
        let t = GitTransport::from_config(&cfg).unwrap();
        assert_eq!(t.local_clone_dir, PathBuf::from("/custom/clone/path"));
    }

    #[test]
    fn tick_errors_with_stub_message() {
        let cfg = cfg_with(
            r#"
[project]
name = "x"
[paths]
wsl_inbox = "/tmp/i"
[transport]
kind = "git"
[transport.git]
state_repo = "git@github.com:mick/x.git"
"#,
        );
        let t = GitTransport::from_config(&cfg).unwrap();
        let err = t.tick(&cfg, "this").unwrap_err();
        assert!(err.to_string().contains("stage-2 stub"));
    }
}
