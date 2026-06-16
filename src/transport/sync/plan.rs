//! Pure sync planner: turn a parsed `Config` + this_host into the list
//! of `SyncCommand`s a tick should issue. No I/O beyond the
//! `local_template.exists()` probe — the planner is testable without
//! actually invoking rsync (the executor lives in `super::rsync`).

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::{Config, Host};

use super::rsync::build_rsync_target;

/// One file to ship to one peer host. Carries enough info to execute the
/// rsync without re-consulting the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCommand {
    pub peer_target: String, // user@tailnet_hostname:path
    pub local_path: PathBuf,
    pub use_append_verify: bool, // true for append-only slice files
    pub kind: &'static str,      // "slice" | "toml" — for logging
}

/// Canonical-config path for the running swarm. Primary source is
/// `cfg.source_path` (set by `Config::load` to the absolute path it
/// loaded from). Falls back to the cross-swarm registry by project
/// name for the rare case where this isn't populated (e.g., a future
/// caller that constructs a Config without using `load`). Finally, as
/// a last resort, a bare `giga-harness.toml` — but quality F13 showed
/// this resolves against CWD and breaks `giga sync --once` invoked
/// from outside the config dir, so the cfg.source_path path should
/// almost always win in practice.
pub(crate) fn cfg_canonical_path(cfg: &Config) -> Result<PathBuf> {
    if let Some(p) = &cfg.source_path {
        return Ok(p.clone());
    }
    if let Some(p) = crate::registry::load().ok().and_then(|r| {
        r.entries
            .into_iter()
            .find(|e| e.name == cfg.project.name)
            .map(|e| e.config)
    }) {
        return Ok(p);
    }
    Ok(PathBuf::from("giga-harness.toml"))
}

/// Pure planner: compute the rsync commands this tick should issue.
/// Inputs: parsed config + this_host name + the canonical config path
/// (for rsync'ing the TOML itself).
///
/// Output rules:
///   - For every PEER host (not this_host), produce one SyncCommand
///     for the canonical TOML.
///   - For every cross-host channel where this_host has at least one
///     participant, produce one SyncCommand per PEER host that has at
///     least one participant on that channel, for THIS host's slice
///     file. Append-verify enabled.
///   - Skip own slice files (never push to self).
///   - Skip local-only channels (no slice exists for them on this host).
pub fn compute_sync_plan(
    cfg: &Config,
    this_host: &str,
    canonical_config_path: &Path,
) -> Vec<SyncCommand> {
    let mut plan = Vec::new();

    let peers: Vec<&Host> = cfg.hosts.iter().filter(|h| h.name != this_host).collect();

    // Local config + inbox dirs — used as the default when a peer
    // hasn't overridden them.
    let local_config_dir = canonical_config_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let local_inbox_dir = cfg
        .paths
        .wsl_inbox
        .clone()
        .or_else(|| cfg.paths.windows_inbox.clone())
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    // 1) Canonical TOML to every peer (at peer's remote_config_dir).
    let toml_filename = canonical_config_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "giga-harness.toml".to_string());
    for peer in &peers {
        let remote_dir = peer
            .remote_config_dir
            .as_ref()
            .cloned()
            .unwrap_or_else(|| local_config_dir.clone());
        let remote_path = crate::foundation::paths::unix_join(&remote_dir, &toml_filename);
        let target = match build_rsync_target(peer, &remote_path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        plan.push(SyncCommand {
            peer_target: target,
            local_path: canonical_config_path.to_path_buf(),
            use_append_verify: false,
            kind: "toml",
        });
    }

    // 1b) agents/<slug>.md templates to every peer. v0.3.2: per-tick
    //     mirror so templates stay in lockstep when add-agent (without
    //     --host) creates a new template after the initial bootstrap
    //     push, OR when AGENTS.md template content is hand-edited.
    //     Cheap (KB scale per agent) and idempotent (rsync no-op when
    //     content matches).
    let templates_subdir = "agents";
    for peer in &peers {
        let remote_dir = peer
            .remote_config_dir
            .as_ref()
            .cloned()
            .unwrap_or_else(|| local_config_dir.clone());
        for agent in &cfg.agents {
            let template_name = format!("{}.md", agent.name);
            let local_template = local_config_dir.join(templates_subdir).join(&template_name);
            if !local_template.exists() {
                continue;
            }
            let remote_template_dir =
                crate::foundation::paths::unix_join(&remote_dir, templates_subdir);
            let remote_template_path = format!("{remote_template_dir}/{template_name}");
            let target = match build_rsync_target(peer, &remote_template_path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            plan.push(SyncCommand {
                peer_target: target,
                local_path: local_template,
                use_append_verify: false, // template content can change wholesale
                kind: "template",
            });
        }
    }

    // 2) Own slice files to every peer that participates on each channel.
    for ch in &cfg.channels {
        if cfg.channel_is_local(ch) {
            continue;
        }
        let mut channel_hosts: Vec<&str> = ch
            .participants
            .iter()
            .filter_map(|p| {
                cfg.agents
                    .iter()
                    .find(|a| a.name == *p)
                    .and_then(|a| cfg.agent_host(a))
            })
            .collect();
        channel_hosts.sort();
        channel_hosts.dedup();

        if !channel_hosts.contains(&this_host) {
            continue;
        }

        let merged_path = match cfg.channel_path(ch) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let slice_path = crate::foundation::slices::slice_path(&merged_path, this_host);
        let slice_filename = slice_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{}.{this_host}.md", ch.file.trim_end_matches(".md")));

        for peer in &peers {
            if !channel_hosts.contains(&peer.name.as_str()) {
                continue;
            }
            // Resolve peer's inbox dir via the central helper. It
            // checks per-host [paths] override, then remote_inbox_dir
            // (v0.2 compat), then global [paths]. Same lookup the peer's
            // own init/channel_path uses, so operator + peer agree.
            let remote_inbox = cfg
                .inbox_for_host_side(Some(&peer.name), &ch.side)
                .unwrap_or_else(|| local_inbox_dir.clone());
            let remote_slice_path =
                crate::foundation::paths::unix_join(&remote_inbox, &slice_filename);
            let target = match build_rsync_target(peer, &remote_slice_path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            plan.push(SyncCommand {
                peer_target: target,
                local_path: slice_path.clone(),
                use_append_verify: true,
                kind: "slice",
            });
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build a 2-host cross-host swarm fixture: alice@wsl-a + bob@wsl-b
    /// + 1 bilateral channel. Returns (tmp, config_path).
    fn fixture(this_host: &str) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let config_path = tmp.path().join("giga-harness.toml");
        let toml = format!(
            r#"
[project]
name = "remote-test"

[paths]
wsl_inbox = '{inbox}'

[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0.ts.net"
ssh_user = "alice"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0.ts.net"
ssh_user = "alice"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "wsl-a"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"
host = "wsl-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox = inbox.to_string_lossy(),
        );
        fs::write(&config_path, toml).unwrap();
        fs::write(
            tmp.path().join("this_host.toml"),
            format!("this_host = \"{this_host}\"\n"),
        )
        .unwrap();
        (tmp, config_path)
    }

    #[test]
    fn plan_pushes_toml_to_every_peer() {
        let (_tmp, config_path) = fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-a", &config_path);
        let toml_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "toml").collect();
        assert_eq!(
            toml_pushes.len(),
            1,
            "one toml push per peer; one peer here"
        );
        // The peer_target uses forward slashes (Linux peer) regardless of
        // operator OS — normalize the expected suffix the same way the
        // production code does before comparing.
        let expected_suffix = config_path.display().to_string().replace('\\', "/");
        assert!(
            toml_pushes[0].peer_target.ends_with(&expected_suffix),
            "peer_target={:?} should end with {:?}",
            toml_pushes[0].peer_target,
            expected_suffix,
        );
        assert!(toml_pushes[0].peer_target.contains("wsl-b.tail0.ts.net"));
        assert!(!toml_pushes[0].use_append_verify, "TOML is whole-file");
    }

    #[test]
    fn plan_pushes_agent_templates_to_every_peer() {
        // v0.3.2 fix for quality finding 2: agents/<slug>.md templates
        // must be in the per-tick push set, so add-agent (without --host)
        // creating a new template after the initial bootstrap stays
        // reflected on peers without requiring a separate manual rsync.
        let (tmp, config_path) = fixture("wsl-a");
        let agents_dir = config_path.parent().unwrap().join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        fs::write(agents_dir.join("alice.md"), b"alice template").unwrap();
        fs::write(agents_dir.join("bob.md"), b"bob template").unwrap();
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-a", &config_path);

        let template_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "template").collect();
        // Fixture has 2 agents (alice, bob), 1 peer (wsl-b) → 2 template pushes.
        assert_eq!(template_pushes.len(), 2, "one template per agent per peer");
        let targets: Vec<&str> = template_pushes
            .iter()
            .map(|c| c.peer_target.as_str())
            .collect();
        assert!(targets.iter().any(|t| t.ends_with("/agents/alice.md")));
        assert!(targets.iter().any(|t| t.ends_with("/agents/bob.md")));
        for cmd in &template_pushes {
            assert!(!cmd.use_append_verify, "templates are whole-file");
            assert!(cmd.peer_target.contains("wsl-b.tail0.ts.net"));
        }
        let _ = tmp; // keep tempdir alive
    }

    #[test]
    fn plan_skips_agent_templates_that_dont_exist_locally() {
        // No agents/ subdir at all → no template entries in plan.
        let (_tmp, config_path) = fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-a", &config_path);
        let template_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "template").collect();
        assert!(
            template_pushes.is_empty(),
            "no templates on disk → no template pushes"
        );
    }

    #[test]
    fn plan_pushes_own_slice_to_peers_on_cross_host_channels() {
        let (tmp, config_path) = fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-a", &config_path);
        let slice_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "slice").collect();
        assert_eq!(
            slice_pushes.len(),
            1,
            "one slice push per peer for the bilateral"
        );
        assert!(slice_pushes[0].use_append_verify, "slices are append-only");
        // We're wsl-a so the slice is alice-bob.wsl-a.md
        assert!(slice_pushes[0]
            .local_path
            .to_string_lossy()
            .ends_with("alice-bob.wsl-a.md"));
        // Target hostname is the peer (wsl-b)
        assert!(slice_pushes[0].peer_target.contains("wsl-b.tail0.ts.net"));
        let _ = tmp; // keep tempdir alive
    }

    #[test]
    fn plan_does_not_push_to_self() {
        let (_tmp, config_path) = fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-a", &config_path);
        for cmd in &plan {
            assert!(
                !cmd.peer_target.contains("wsl-a.tail0.ts.net"),
                "should never push to own host: {cmd:?}"
            );
        }
    }

    #[test]
    fn plan_symmetric_from_other_host_pushes_other_slice() {
        // Same swarm, viewed from wsl-b's perspective: it should push
        // its own (wsl-b) slice to wsl-a, not wsl-a's slice.
        let (_tmp, config_path) = fixture("wsl-b");
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-b", &config_path);
        let slice_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "slice").collect();
        assert_eq!(slice_pushes.len(), 1);
        assert!(slice_pushes[0]
            .local_path
            .to_string_lossy()
            .ends_with("alice-bob.wsl-b.md"));
        assert!(slice_pushes[0].peer_target.contains("wsl-a.tail0.ts.net"));
    }

    #[test]
    fn plan_skips_local_only_channels() {
        // Re-write the fixture so bob also lives on wsl-a -> channel is
        // local-only -> no slice push for it.
        let (tmp, config_path) = fixture("wsl-a");
        let body = fs::read_to_string(&config_path)
            .unwrap()
            .replace(r#"host = "wsl-b""#, r#"host = "wsl-a""#);
        fs::write(&config_path, body).unwrap();
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-a", &config_path);
        let slice_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "slice").collect();
        assert!(
            slice_pushes.is_empty(),
            "local-only channels need no slice push"
        );
        // TOML push still happens (peer might have other reasons to receive).
        let toml_pushes: Vec<_> = plan.iter().filter(|c| c.kind == "toml").collect();
        assert_eq!(toml_pushes.len(), 1);
        let _ = tmp;
    }

    #[test]
    fn plan_uses_peer_remote_config_dir_override_when_set() {
        // When the local config lives at /home/alice/... and the peer's
        // config lives at /home/bob/... (different user, different
        // $HOME), the toml push must target the peer's path, not the
        // local path. v1.1 fix for the homogeneous-path-assumption bug
        // surfaced in the live smoke (REMOTE_DESIGN.md §6 step 10).
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let local_cfg = tmp.path().join("local").join("giga-harness.toml");
        fs::create_dir_all(local_cfg.parent().unwrap()).unwrap();
        fs::write(
            &local_cfg,
            format!(
                r#"
[project]
name = "x"

[paths]
wsl_inbox = '{inbox}'

[[hosts]]
name = "self"
tailnet_hostname = "self.tail0.ts.net"

[[hosts]]
name = "peer"
tailnet_hostname = "peer.tail0.ts.net"
ssh_user = "bob"
remote_config_dir = "/home/bob/.giga/configs/x"
remote_inbox_dir = "/home/bob/projects/inbox"
"#,
                inbox = inbox.to_string_lossy(),
            ),
        )
        .unwrap();
        fs::write(
            tmp.path().join("local").join("this_host.toml"),
            "this_host = \"self\"\n",
        )
        .unwrap();
        let cfg = Config::load(&local_cfg).unwrap();
        let plan = compute_sync_plan(&cfg, "self", &local_cfg);
        let toml = plan.iter().find(|c| c.kind == "toml").expect("toml push");
        assert_eq!(
            toml.peer_target,
            "bob@peer.tail0.ts.net:/home/bob/.giga/configs/x/giga-harness.toml"
        );
    }

    #[test]
    fn plan_uses_peer_remote_inbox_dir_override_when_set_for_slice_push() {
        // Same idea — slice files land in the peer's remote_inbox_dir
        // when set, not at the local inbox path.
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let local_cfg = tmp.path().join("local").join("giga-harness.toml");
        fs::create_dir_all(local_cfg.parent().unwrap()).unwrap();
        fs::write(
            &local_cfg,
            format!(
                r#"
[project]
name = "x"

[paths]
wsl_inbox = '{inbox}'

[[hosts]]
name = "self"
tailnet_hostname = "self.tail0.ts.net"

[[hosts]]
name = "peer"
tailnet_hostname = "peer.tail0.ts.net"
ssh_user = "bob"
remote_inbox_dir = "/home/bob/projects/inbox"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "self"

[[agents]]
name = "bob-agent"
workdir = "/h/bob-agent"
role = "."
platform = "wsl"
host = "peer"

[[channels]]
file = "alice-bob-agent.md"
side = "wsl"
participants = ["alice", "bob-agent"]
"#,
                inbox = inbox.to_string_lossy(),
            ),
        )
        .unwrap();
        fs::write(
            tmp.path().join("local").join("this_host.toml"),
            "this_host = \"self\"\n",
        )
        .unwrap();
        let cfg = Config::load(&local_cfg).unwrap();
        let plan = compute_sync_plan(&cfg, "self", &local_cfg);
        let slice = plan
            .iter()
            .find(|c| c.kind == "slice")
            .expect("slice push to peer");
        assert!(
            slice
                .peer_target
                .ends_with("/home/bob/projects/inbox/alice-bob-agent.self.md"),
            "expected peer_target to end with peer's inbox dir + slice filename, got: {}",
            slice.peer_target
        );
        assert!(slice.use_append_verify);
    }

    #[test]
    fn plan_with_no_peers_is_empty() {
        // Single-host swarm with [[hosts]] entry — degenerate but valid.
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let config_path = tmp.path().join("giga-harness.toml");
        let toml = format!(
            r#"
[project]
name = "solo"

[paths]
wsl_inbox = '{inbox}'

[[hosts]]
name = "wsl-only"
tailnet_hostname = "wsl-only.tail0.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "wsl-only"
"#,
            inbox = inbox.to_string_lossy(),
        );
        fs::write(&config_path, toml).unwrap();
        fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"wsl-only\"\n",
        )
        .unwrap();
        let cfg = Config::load(&config_path).unwrap();
        let plan = compute_sync_plan(&cfg, "wsl-only", &config_path);
        assert!(plan.is_empty());
    }

    /// v0.3.4 fix for quality finding 13: when sync runs via `tick_once`
    /// (the entry point for the rsync_tailscale transport's tick), the
    /// canonical config path must come from `cfg.source_path` (the
    /// absolute path Config::load read from) — NOT a CWD-relative bare
    /// filename. Quality's repro: `giga sync --once` from $HOME failed
    /// with rsync source `link_stat "$HOME/giga-harness.toml" failed`
    /// because the fallback was CWD-relative.
    #[test]
    fn cfg_canonical_path_uses_config_source_path_not_cwd() {
        let (_tmp, config_path) = fixture("wsl-a");
        let cfg = Config::load(&config_path).unwrap();
        assert!(
            cfg.source_path.is_some(),
            "Config::load must populate source_path"
        );
        let resolved = cfg_canonical_path(&cfg).unwrap();
        assert!(
            resolved.is_absolute(),
            "canonical path must be absolute, got {resolved:?}"
        );
        // canonicalize() may resolve symlinks (e.g. /var -> /private/var on macOS),
        // so compare against the canonicalized fixture path too.
        let expected = std::fs::canonicalize(&config_path).unwrap_or(config_path.clone());
        assert_eq!(resolved, expected);
    }

    /// v0.4.2 Bug 11 fix: the daemon loop reloads cfg every
    /// RELOAD_EVERY_N_TICKS. This test simulates the reload semantic
    /// by computing a sync plan against a config, then mutating the
    /// file, then reloading + recomputing the plan, and asserting the
    /// post-mutation plan reflects the new channel. Pre-fix the
    /// daemon would have kept iterating the original snapshot
    /// forever; this asserts the fix's contract end-to-end at the
    /// data-flow level.
    #[test]
    fn config_reload_picks_up_newly_added_channel_at_runtime() {
        let (_tmp, config_path) = fixture("wsl-a");
        let cfg_before = Config::load(&config_path).unwrap();
        let plan_before = compute_sync_plan(&cfg_before, "wsl-a", &config_path);
        let slice_pushes_before: Vec<_> =
            plan_before.iter().filter(|c| c.kind == "slice").collect();
        assert_eq!(
            slice_pushes_before.len(),
            1,
            "fixture starts with 1 cross-host channel"
        );

        // Mutate the on-disk config to add another cross-host channel
        // (simulates `giga add-channel` or `giga add-agent` after the
        // daemon launched).
        let mut text = fs::read_to_string(&config_path).unwrap();
        text.push_str(
            r#"
[[agents]]
name = "carol"
workdir = "/h/carol"
role = "."
platform = "wsl"
host = "wsl-b"

[[channels]]
file = "alice-carol.md"
side = "wsl"
participants = ["alice", "carol"]
"#,
        );
        fs::write(&config_path, text).unwrap();

        let cfg_after = Config::load(&config_path).unwrap();
        let plan_after = compute_sync_plan(&cfg_after, "wsl-a", &config_path);
        let slice_pushes_after: Vec<_> = plan_after.iter().filter(|c| c.kind == "slice").collect();
        assert_eq!(
            slice_pushes_after.len(),
            2,
            "post-reload plan must include the newly-added channel's slice push"
        );
        assert!(
            slice_pushes_after.iter().any(|c| c
                .local_path
                .to_string_lossy()
                .ends_with("alice-carol.wsl-a.md")),
            "new channel `alice-carol.md` slice push missing from reloaded plan"
        );
    }
}
