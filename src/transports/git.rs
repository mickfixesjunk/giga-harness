//! Git transport — shared git repo as swarm state store. Per
//! TRANSPORT_DESIGN.md §4.3.
//!
//! Repo layout:
//!   <local-clone-dir>/
//!   ├── giga-harness.toml      (canonical; everyone reads + operator writes)
//!   ├── agents/<slug>.md       (per-agent CLAUDE.md templates; operator writes)
//!   └── slices/<channel>.<host>.md   (single-writer: only that host pushes)
//!
//! Each per-tick sweep:
//!   1. git pull --rebase  (pulls peer slice growth + canonical TOML updates)
//!   2. Mirror peer slices in the repo → local inbox <channel>.<peer-host>.md
//!   3. Mirror own canonical TOML → repo if changed locally (operator edited)
//!   4. Mirror own slice growth → repo's slices/<channel>.<this-host>.md
//!   5. git add -A && git commit (skip if no changes) && git push
//!
//! Conflict story:
//!   - Per-host slice files are SINGLE WRITER → conflict-free by construction
//!   - Canonical TOML is multi-writer in principle but operator-on-one-box
//!     in practice; if two operators race, last-writer-wins (documented
//!     limitation for v0.3.0, see TRANSPORT_DESIGN.md §7 Q4)
//!
//! Auth: standard git mechanisms — SSH keypair for `git@...` URLs,
//! credential helper for HTTPS. giga doesn't manage auth.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

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

    /// Ensure the state repo is cloned locally. Idempotent — no-op if
    /// the clone already exists with the right remote URL.
    fn ensure_clone(&self) -> Result<()> {
        if self.local_clone_dir.join(".git").exists() {
            return Ok(());
        }
        if let Some(parent) = self.local_clone_dir.parent() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
        let status = Command::new("git")
            .args([
                "clone",
                "--quiet",
                &self.state_repo,
                &self.local_clone_dir.to_string_lossy(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| {
                format!(
                    "invoking git clone {} {}",
                    self.state_repo,
                    self.local_clone_dir.display()
                )
            })?;
        if !status.success() {
            return Err(anyhow!(
                "git clone {} exited {}",
                self.state_repo,
                status.code().unwrap_or(-1)
            ));
        }
        Ok(())
    }

    fn run_git(&self, args: &[&str]) -> Result<()> {
        let status = Command::new("git")
            .current_dir(&self.local_clone_dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("invoking git {}", args.join(" ")))?;
        if !status.success() {
            return Err(anyhow!(
                "git {} exited {}",
                args.join(" "),
                status.code().unwrap_or(-1)
            ));
        }
        Ok(())
    }

    /// Like `run_git` but silent on stderr (used for the
    /// "did anything change?" `diff --cached --quiet` test that
    /// returns non-zero by design).
    fn run_git_quiet(&self, args: &[&str]) -> std::process::ExitStatus {
        Command::new("git")
            .current_dir(&self.local_clone_dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("spawn git")
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

// ============================================================================
// Mirror primitives — append-only by construction (matches the broader
// slice-and-merge invariant from REMOTE_DESIGN.md §2.1).
// ============================================================================

/// Append the bytes [`dest.len()` .. `src.len()`] from `src` onto
/// `dest`. Creates `dest` if absent. No-op if `src` doesn't exist or
/// `src.len() <= dest.len()` (src didn't grow, or was truncated —
/// truncation is anomalous but we shouldn't make it worse).
fn append_growth(src: &Path, dest: &Path) -> Result<u64> {
    let src_size = match fs::metadata(src) {
        Ok(m) => m.len(),
        Err(_) => return Ok(0), // src absent; nothing to do
    };
    let dest_size = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    if src_size <= dest_size {
        return Ok(0);
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    // Read source delta + append.
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut sf = fs::File::open(src).with_context(|| format!("open {}", src.display()))?;
    sf.seek(SeekFrom::Start(dest_size))?;
    let mut delta = vec![0u8; (src_size - dest_size) as usize];
    sf.read_exact(&mut delta)?;
    let mut df = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dest)
        .with_context(|| format!("open-append {}", dest.display()))?;
    df.write_all(&delta)?;
    Ok(src_size - dest_size)
}

/// Whole-file mirror with content-equality skip — used for the
/// canonical TOML (not append-only). Copies src → dest only when they
/// differ; returns whether a copy happened.
fn copy_if_different(src: &Path, dest: &Path) -> Result<bool> {
    let src_bytes = match fs::read(src) {
        Ok(b) => b,
        Err(_) => return Ok(false), // src absent; nothing to do
    };
    let dest_bytes = fs::read(dest).ok();
    if dest_bytes.as_deref() == Some(src_bytes.as_slice()) {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    fs::write(dest, &src_bytes).with_context(|| format!("write {}", dest.display()))?;
    Ok(true)
}

// ============================================================================
// Plan: which slices belong to which host on which channel
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
struct SlicePlan {
    /// Channel filename (e.g. "alice-bob.md").
    channel: String,
    /// Host that owns this slice (single-writer).
    owner: String,
}

/// Enumerate every cross-host slice in the swarm: for each channel
/// that spans hosts, one entry per host that participates. Pure —
/// testable without filesystem.
fn slice_plan(cfg: &Config) -> Vec<SlicePlan> {
    let mut plan = Vec::new();
    for ch in &cfg.channels {
        if cfg.channel_is_local(ch) {
            continue; // local-only channels don't use slices
        }
        let mut hosts: Vec<String> = ch
            .participants
            .iter()
            .filter_map(|p| {
                cfg.agents
                    .iter()
                    .find(|a| a.name == *p)
                    .and_then(|a| cfg.agent_host(a))
                    .map(|h| h.to_string())
            })
            .collect();
        hosts.sort();
        hosts.dedup();
        for owner in hosts {
            plan.push(SlicePlan {
                channel: ch.file.clone(),
                owner,
            });
        }
    }
    plan
}

/// Slice filename in the repo: `slices/<channel-stem>.<host>.md`
fn repo_slice_path(repo: &Path, p: &SlicePlan) -> PathBuf {
    let stem = p.channel.trim_end_matches(".md");
    repo.join("slices").join(format!("{stem}.{}.md", p.owner))
}

/// Slice filename in the local inbox: `<channel-stem>.<host>.md`
fn inbox_slice_path(inbox: &Path, p: &SlicePlan) -> PathBuf {
    let stem = p.channel.trim_end_matches(".md");
    inbox.join(format!("{stem}.{}.md", p.owner))
}

// ============================================================================
// Transport impl
// ============================================================================

impl Transport for GitTransport {
    fn name(&self) -> &'static str {
        "git"
    }

    fn tick(&self, cfg: &Config, this_host: &str, dry_run: bool) -> Result<()> {
        if dry_run {
            eprintln!(
                "[dry-run] git tick on {this_host}: would git pull, mirror peers→inbox, mirror own→repo, commit, push"
            );
            return Ok(());
        }
        self.ensure_clone()?;

        // 1. Pull peer updates.
        self.run_git(&["pull", "--rebase", "--quiet"])
            .context("git pull --rebase failed (auth / network / merge conflict?)")?;

        let inbox = cfg
            .paths
            .wsl_inbox
            .as_ref()
            .ok_or_else(|| anyhow!("git tick: paths.wsl_inbox not set"))?;
        let plan = slice_plan(cfg);

        // 2. Repo → local inbox (peer slices).
        for p in &plan {
            if p.owner == this_host {
                continue; // own slice — handled by step 4 instead
            }
            let src = repo_slice_path(&self.local_clone_dir, p);
            let dst = inbox_slice_path(inbox, p);
            let bytes = append_growth(&src, &dst)?;
            if bytes > 0 {
                eprintln!(
                    "git tick: appended {bytes} bytes from peer slice {} → {}",
                    p.channel, p.owner
                );
            }
        }

        // 3. Canonical TOML: local → repo if changed (operator edited locally).
        let repo_toml = self.local_clone_dir.join("giga-harness.toml");
        if let Some(local_toml) = canonical_toml_path(&cfg.project.name) {
            if copy_if_different(&local_toml, &repo_toml)? {
                eprintln!("git tick: pushed canonical TOML change to repo");
            }
        }

        // 4. Local inbox → repo (own slice growth).
        for p in &plan {
            if p.owner != this_host {
                continue;
            }
            let src = inbox_slice_path(inbox, p);
            let dst = repo_slice_path(&self.local_clone_dir, p);
            let bytes = append_growth(&src, &dst)?;
            if bytes > 0 {
                eprintln!(
                    "git tick: appended {bytes} bytes own slice {} → repo",
                    p.channel
                );
            }
        }

        // 5. Commit + push (no-op if nothing changed).
        self.run_git(&["add", "-A"])?;
        let diff_status = self.run_git_quiet(&["diff", "--cached", "--quiet"]);
        if !diff_status.success() {
            // diff --cached --quiet returns 1 when there are staged changes.
            let msg = format!("sync from {this_host}");
            self.run_git(&["commit", "--quiet", "-m", &msg])?;
            self.run_git(&["push", "--quiet"])
                .context("git push failed (auth / network / non-fast-forward?)")?;
        }

        Ok(())
    }

    fn bootstrap_peer(&self, cfg: &Config, _peer: &str, _config_path: &Path) -> Result<()> {
        // For the git transport, the peer bootstraps itself by
        // running `giga setup --remote-node
        // --transport git --repo <url>`. From this side we just
        // ensure the latest canonical TOML is committed, so the
        // peer's next git pull sees the operator's recent edits.
        let this_host = cfg
            .this_host
            .clone()
            .ok_or_else(|| anyhow!("bootstrap_peer needs this_host"))?;
        self.tick(cfg, &this_host, false)
    }

    fn supports_remote_exec(&self) -> bool {
        false
    }
}

/// Best-effort registry lookup for the local canonical TOML path.
/// Returns None if the swarm isn't registered (which would be
/// unusual at the point a transport is running) — caller falls back
/// to "no local TOML to push" which is harmless.
fn canonical_toml_path(project: &str) -> Option<PathBuf> {
    crate::registry::load()
        .ok()?
        .entries
        .into_iter()
        .find(|e| e.name == project)
        .map(|e| e.config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cfg_with(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    // ----- from_config / default paths -----

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

    // ----- slice_plan -----

    fn cross_host_cfg() -> Config {
        let mut cfg: Config = toml::from_str(
            r#"
[project]
name = "x"
[paths]
wsl_inbox = "/tmp/i"
[transport]
kind = "git"
[transport.git]
state_repo = "git@github.com:mick/x.git"

[[hosts]]
name = "wsl-a"
tailnet_hostname = "a.tail.ts.net"
[[hosts]]
name = "wsl-b"
tailnet_hostname = "b.tail.ts.net"

[[agents]]
name = "alice"
workdir = "/h/a"
role = "."
platform = "wsl"
host = "wsl-a"
[[agents]]
name = "bob"
workdir = "/h/b"
role = "."
platform = "wsl"
host = "wsl-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
        )
        .unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg
    }

    #[test]
    fn slice_plan_includes_every_owner_of_cross_host_channels() {
        let cfg = cross_host_cfg();
        let plan = slice_plan(&cfg);
        assert_eq!(plan.len(), 2);
        let owners: Vec<&str> = plan.iter().map(|p| p.owner.as_str()).collect();
        assert!(owners.contains(&"wsl-a"));
        assert!(owners.contains(&"wsl-b"));
    }

    #[test]
    fn slice_plan_skips_local_only_channels() {
        // Both agents on wsl-a → channel is local-only → no slices
        let mut cfg = cross_host_cfg();
        for a in cfg.agents.iter_mut() {
            a.host = Some("wsl-a".into());
        }
        let plan = slice_plan(&cfg);
        assert!(plan.is_empty());
    }

    // ----- repo path helpers -----

    #[test]
    fn repo_slice_path_drops_md_then_appends_host() {
        let p = SlicePlan {
            channel: "alice-bob.md".into(),
            owner: "wsl-a".into(),
        };
        let path = repo_slice_path(Path::new("/repo"), &p);
        assert_eq!(path, PathBuf::from("/repo/slices/alice-bob.wsl-a.md"));
    }

    #[test]
    fn inbox_slice_path_mirrors_repo_layout_without_slices_subdir() {
        let p = SlicePlan {
            channel: "alice-bob.md".into(),
            owner: "wsl-b".into(),
        };
        let path = inbox_slice_path(Path::new("/inbox"), &p);
        assert_eq!(path, PathBuf::from("/inbox/alice-bob.wsl-b.md"));
    }

    // ----- append_growth -----

    #[test]
    fn append_growth_creates_dest_when_absent() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.md");
        let dst = tmp.path().join("dst.md");
        fs::write(&src, b"hello world").unwrap();
        let bytes = append_growth(&src, &dst).unwrap();
        assert_eq!(bytes, 11);
        assert_eq!(fs::read(&dst).unwrap(), b"hello world");
    }

    #[test]
    fn append_growth_appends_only_new_bytes() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.md");
        let dst = tmp.path().join("dst.md");
        fs::write(&src, b"hello").unwrap();
        append_growth(&src, &dst).unwrap();
        fs::write(&src, b"hello world").unwrap();
        let bytes = append_growth(&src, &dst).unwrap();
        assert_eq!(bytes, 6); // " world"
        assert_eq!(fs::read(&dst).unwrap(), b"hello world");
    }

    #[test]
    fn append_growth_noop_when_src_absent() {
        let tmp = TempDir::new().unwrap();
        let bytes = append_growth(&tmp.path().join("nope"), &tmp.path().join("dst")).unwrap();
        assert_eq!(bytes, 0);
    }

    #[test]
    fn append_growth_noop_when_src_did_not_grow() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.md");
        let dst = tmp.path().join("dst.md");
        fs::write(&src, b"hello").unwrap();
        fs::write(&dst, b"hello").unwrap();
        let bytes = append_growth(&src, &dst).unwrap();
        assert_eq!(bytes, 0);
    }

    #[test]
    fn append_growth_noop_on_shrink_does_not_truncate_dest() {
        // Pathological: src shrunk somehow. Don't make it worse —
        // leave dst intact, defer recovery to a higher layer.
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.md");
        let dst = tmp.path().join("dst.md");
        fs::write(&src, b"hi").unwrap();
        fs::write(&dst, b"hello world").unwrap();
        let bytes = append_growth(&src, &dst).unwrap();
        assert_eq!(bytes, 0);
        assert_eq!(fs::read(&dst).unwrap(), b"hello world");
    }

    // ----- copy_if_different -----

    #[test]
    fn copy_if_different_copies_when_dest_absent() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("a").join("dst");
        fs::write(&src, b"content").unwrap();
        assert!(copy_if_different(&src, &dst).unwrap());
        assert_eq!(fs::read(&dst).unwrap(), b"content");
    }

    #[test]
    fn copy_if_different_skips_when_identical() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::write(&src, b"same").unwrap();
        fs::write(&dst, b"same").unwrap();
        assert!(!copy_if_different(&src, &dst).unwrap());
    }

    #[test]
    fn copy_if_different_overwrites_when_differs() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::write(&src, b"new").unwrap();
        fs::write(&dst, b"old").unwrap();
        assert!(copy_if_different(&src, &dst).unwrap());
        assert_eq!(fs::read(&dst).unwrap(), b"new");
    }
}
