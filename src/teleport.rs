//! `giga teleport` — move an agent from one host to another in the tailnet.
//!
//! See TELEPORT_DESIGN.md for the architecture.
//!
//! What moves:
//!   - TOML `agent.host` field (atomic edit, sync'd to peers)
//!   - Agent's workdir contents (rsync source→target over tailnet SSH)
//!   - HANDOVER.md gets a "you have been teleported" banner prepended
//!     so the agent sees it as the first content of its next session
//!   - Old tmux session killed gracefully on the source host
//!   - New tmux session launched on the target host
//!
//! What does NOT move:
//!   - Channel slice files. Past posts stay in `<channel>.<source>.md`
//!     forever (still visible swarm-wide via merge); new posts go to
//!     `<channel>.<target>.md`. Append-only invariant preserved.
//!   - `~/.claude/` conversation history (per-machine). The agent
//!     restarts on the target; HANDOVER.md carries context.
//!   - Giga cursors (per-machine; reset on target → first watch tick
//!     auto-replays history from byte 0, agent gets a backlog dump).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use toml_edit::{value, DocumentMut};

use crate::config::{Config, Host};
use crate::sync;

pub struct Args {
    pub agent: String,
    pub to: String,
    /// Optional explicit source host. When omitted, defaults to the
    /// agent's current `host` field in the TOML.
    pub from: Option<String>,
    /// Don't kill the source pane after the new pane is up. Operator
    /// will tear it down manually after verifying the target side.
    pub keep_running: bool,
    /// Print every step that would be taken; no side effects.
    pub dry_run: bool,
    pub config: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    let cfg = Config::load(&args.config)?;

    let plan = preflight(&cfg, &args)?;

    if args.dry_run {
        print_dry_run(&plan);
        return Ok(());
    }

    let abs_config = std::fs::canonicalize(&args.config).unwrap_or(args.config.clone());

    println!("==> teleporting `{}` from `{}` to `{}`", plan.agent, plan.source.name, plan.target.name);

    // 1. ensure HANDOVER.md exists on source (touch if missing) so
    //    rsync has something to carry over.
    let source_ssh = build_ssh_target(&plan.source)?;
    let source_handover_unix = format!(
        "{}/HANDOVER.md",
        sync::remote_join(&plan.source_workdir, "").trim_end_matches('/'),
    );
    let escaped_handover =
        shell_escape::unix::escape(std::borrow::Cow::Borrowed(source_handover_unix.as_str()));
    sync::ssh_run(
        &source_ssh,
        &format!("touch {escaped_handover}"),
    )
    .context("ensuring source HANDOVER.md exists before rsync")?;
    println!("  + source HANDOVER.md exists (touched if missing)");

    // 2. rsync workdir from source to target (direct A→B over tailnet SSH).
    rsync_direct(&plan)?;
    println!("  + workdir rsynced source -> target");

    // 3. Prepend the teleport banner on the target's HANDOVER.md.
    prepend_banner_on_target(&plan)?;
    println!("  + HANDOVER.md banner prepended on target");

    // 4. Update TOML: agent.host = target. Atomic write to canonical.
    update_toml_agent_host(&args.config, &plan.agent, &plan.target.name)?;
    println!("  + TOML updated: agent.host = `{}`", plan.target.name);

    // 5. Sync TOML to peers (best-effort; one tick).
    if let Err(e) = sync_toml_to_peers(&abs_config) {
        eprintln!("  ! sync to peers failed ({e:#}) — agents on peers may take ~15s to see the host change via the periodic reload");
    } else {
        println!("  + TOML sync'd to peers");
    }

    // 6. SSH target: giga init --only <agent>.
    if let Err(e) = run_remote_giga(&plan.target, &abs_config, &["init"])
        .context("giga init on target")
    {
        eprintln!("  ! init on target failed ({e:#}) — run `giga remote --host {} -- init` manually", plan.target.name);
    } else {
        println!("  + giga init complete on target");
    }

    // 7. SSH target: giga launch --only <agent>.
    if let Err(e) = run_remote_giga(
        &plan.target,
        &abs_config,
        &["launch", "--only", &plan.agent],
    )
    .context("giga launch on target")
    {
        eprintln!("  ! launch on target failed ({e:#}) — run `giga remote --host {} -- launch --only {}` manually", plan.target.name, plan.agent);
    } else {
        println!("  + giga launch complete on target");
    }

    // 8. Kill old pane on source (unless --keep-running). For codex
    //    agents that's 2 panes (<agent>-cli + <agent>-bridge); for
    //    claude/agy it's 1 pane (<agent>). kill_old_pane fires kill
    //    against all three possible names so we don't need to know the
    //    runtime at kill time.
    if args.keep_running {
        println!();
        println!("(--keep-running: source pane(s) left alive on `{}`. Tear down manually with:", plan.source.name);
        println!("  # claude/agy agents (1 pane):");
        println!("  giga remote --host {} -- bash -lc 'tmux kill-window -t giga-{}:{}'", plan.source.name, cfg.project.name, plan.agent);
        println!("  # codex agents (2 panes):");
        println!("  giga remote --host {} -- bash -lc 'tmux kill-window -t giga-{}:{}-cli; tmux kill-window -t giga-{}:{}-bridge'", plan.source.name, cfg.project.name, plan.agent, cfg.project.name, plan.agent);
        println!(")");
    } else {
        match kill_old_pane(&plan.source, &cfg.project.name, &plan.agent) {
            Ok(()) => println!("  + source pane(s) killed on `{}`", plan.source.name),
            Err(e) => eprintln!("  ! source pane kill failed ({e:#}) — verify with `giga remote --host {} -- bash -lc 'tmux list-windows -t giga-{}'`", plan.source.name, cfg.project.name),
        }
    }

    println!();
    println!("teleport complete. Verify with: giga hosts");
    Ok(())
}

/// Captured + validated teleport plan. Built by preflight so the
/// execution path has no remaining "could this fail validation"
/// branches.
#[derive(Debug)]
struct Plan<'a> {
    agent: String,
    source: &'a Host,
    target: &'a Host,
    source_workdir: PathBuf,
    target_workdir: PathBuf,
}

fn preflight<'a>(cfg: &'a Config, args: &Args) -> Result<Plan<'a>> {
    let agent = cfg
        .agents
        .iter()
        .find(|a| a.name == args.agent)
        .ok_or_else(|| {
            anyhow!(
                "agent `{}` not in [[agents]] (known: {:?})",
                args.agent,
                cfg.agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            )
        })?;
    let source_name = args
        .from
        .clone()
        .or_else(|| agent.host.clone())
        .ok_or_else(|| {
            anyhow!(
                "couldn't determine source host for `{}` — pass --from <host> or add `host = \"...\"` to the agent's TOML entry",
                args.agent,
            )
        })?;
    if source_name == args.to {
        return Err(anyhow!(
            "source and target hosts are both `{}` — nothing to teleport",
            source_name,
        ));
    }
    let source = cfg
        .hosts
        .iter()
        .find(|h| h.name == source_name)
        .ok_or_else(|| {
            anyhow!(
                "source host `{}` not in [[hosts]] (known: {:?})",
                source_name,
                cfg.hosts.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            )
        })?;
    let target = cfg
        .hosts
        .iter()
        .find(|h| h.name == args.to)
        .ok_or_else(|| {
            anyhow!(
                "target host `{}` not in [[hosts]] (known: {:?})",
                args.to,
                cfg.hosts.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            )
        })?;
    // Workdir path is the SAME on both hosts under the homogeneous-path
    // assumption (per REMOTE_DESIGN.md). If a swarm has heterogeneous
    // paths, they need a per-host workdir override field on agents —
    // out of scope for v1. We use agent.workdir verbatim for both
    // sides.
    let workdir = agent.workdir.clone();
    Ok(Plan {
        agent: args.agent.clone(),
        source,
        target,
        source_workdir: workdir.clone(),
        target_workdir: workdir,
    })
}

fn print_dry_run(plan: &Plan) {
    println!("dry-run: would teleport `{}` from `{}` to `{}`", plan.agent, plan.source.name, plan.target.name);
    println!("  1. SSH {} : touch {}/HANDOVER.md (ensure exists)", plan.source.name, plan.source_workdir.display());
    println!("  2. SSH {} : rsync -avz {}/ {}:{}/", plan.source.name, plan.source_workdir.display(), plan.target.name, plan.target_workdir.display());
    println!("  3. SSH {} : prepend teleport banner to HANDOVER.md", plan.target.name);
    println!("  4. local: edit TOML agent.host = `{}`", plan.target.name);
    println!("  5. local: giga sync --once (push TOML to peers)");
    println!("  6. SSH {} : giga init", plan.target.name);
    println!("  7. SSH {} : giga launch --only {}", plan.target.name, plan.agent);
    println!("  8. SSH {} : tmux kill-window (graceful: SIGTERM + 5s + kill)", plan.source.name);
    println!("(dry-run — no side effects)");
}

/// Build the `user@tailnet` SSH target string for a host. Mirrors the
/// rsync target builder but without the trailing `:path` part.
pub(crate) fn build_ssh_target(host: &Host) -> Result<String> {
    let user = host
        .ssh_user
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .ok_or_else(|| {
            anyhow!(
                "can't determine SSH user for host `{}` (no ssh_user; $USER and $USERNAME both unset)",
                host.name,
            )
        })?;
    Ok(format!("{user}@{}", host.tailnet_hostname))
}

/// rsync the agent's workdir from the source host to the target host,
/// direct over tailnet SSH (operator SSHes to source, source rsyncs to
/// target). Falls back to two-hop via operator if direct fails.
fn rsync_direct(plan: &Plan) -> Result<()> {
    let source_ssh = build_ssh_target(plan.source)?;
    let target_rsync_target = sync::build_rsync_target(
        plan.target,
        &format!("{}/", plan.target_workdir.display()),
    )?;
    let source_workdir_unix = format!("{}/", plan.source_workdir.display());
    // `rsync -avz --delete-after` so files removed locally on the source
    // since last sync are removed on the target too. `--delete-after`
    // (not `--delete`) so deletions happen after the transfer succeeds
    // — half-failed runs don't lose data.
    let escaped_src =
        shell_escape::unix::escape(std::borrow::Cow::Borrowed(source_workdir_unix.as_str()));
    let escaped_tgt =
        shell_escape::unix::escape(std::borrow::Cow::Borrowed(target_rsync_target.as_str()));
    let remote_cmd = format!(
        "rsync -avz --delete-after {escaped_src} {escaped_tgt}",
    );
    let direct_result = sync::ssh_run(&source_ssh, &remote_cmd);
    if direct_result.is_ok() {
        return Ok(());
    }
    eprintln!(
        "  ! direct A->B rsync failed ({:#}); falling back to two-hop via operator",
        direct_result.unwrap_err(),
    );
    rsync_two_hop(plan)
}

/// Fallback: rsync source workdir to a local tempdir on the operator,
/// then rsync from operator to target. Slower; always works if
/// operator can reach both endpoints.
fn rsync_two_hop(plan: &Plan) -> Result<()> {
    // No-dep staging dir under std::env::temp_dir. We don't clean up
    // explicitly — staging persists across run for forensics if the
    // hop fails. Acceptable: rsync is content-addressed so re-runs
    // don't accumulate.
    let base = std::env::temp_dir().join(format!(
        "giga-teleport-{}-{}",
        plan.agent,
        std::process::id()
    ));
    std::fs::create_dir_all(&base)
        .with_context(|| format!("creating operator-side staging dir {}", base.display()))?;
    let local_staging = base.join("workdir");

    // Hop 1: source -> operator
    let source_rsync_target = sync::build_rsync_target(
        plan.source,
        &format!("{}/", plan.source_workdir.display()),
    )?;
    let status = Command::new("rsync")
        .args([
            "-avz",
            &source_rsync_target,
            local_staging.to_str().ok_or_else(|| anyhow!("non-UTF8 staging path"))?,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("rsync source -> operator")?;
    if !status.success() {
        return Err(anyhow!(
            "rsync source -> operator exited {}",
            status.code().unwrap_or(-1),
        ));
    }

    // Hop 2: operator -> target
    let target_rsync_target = sync::build_rsync_target(
        plan.target,
        &format!("{}/", plan.target_workdir.display()),
    )?;
    let local_staging_with_slash = format!("{}/", local_staging.display());
    let status = Command::new("rsync")
        .args([
            "-avz",
            "--delete-after",
            &local_staging_with_slash,
            &target_rsync_target,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("rsync operator -> target")?;
    if !status.success() {
        return Err(anyhow!(
            "rsync operator -> target exited {}",
            status.code().unwrap_or(-1),
        ));
    }
    Ok(())
}

/// Prepend the teleport banner to HANDOVER.md on the target. SSH-side
/// shell trick: read old content, write banner + old content back.
fn prepend_banner_on_target(plan: &Plan) -> Result<()> {
    let target_ssh = build_ssh_target(plan.target)?;
    let handover_path = format!("{}/HANDOVER.md", plan.target_workdir.display());
    let escaped_path =
        shell_escape::unix::escape(std::borrow::Cow::Borrowed(handover_path.as_str()));
    let banner = render_teleport_banner(&plan.source.name, &plan.target.name);
    let escaped_banner = shell_escape::unix::escape(std::borrow::Cow::Borrowed(banner.as_str()));
    // `printf '%s' <banner>; [existing content]` written back atomically
    // via a temp file + mv. Avoids race where a partial read+write loses
    // content.
    let cmd = format!(
        "tmp=$(mktemp); \
         printf '%s' {escaped_banner} > \"$tmp\"; \
         if [ -f {escaped_path} ]; then cat {escaped_path} >> \"$tmp\"; fi; \
         mv \"$tmp\" {escaped_path}",
    );
    sync::ssh_run(&target_ssh, &cmd).context("prepending teleport banner on target")
}

/// Render the teleport banner block. Pure fn for testability.
pub(crate) fn render_teleport_banner(source_host: &str, target_host: &str) -> String {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    format!(
        "> **You have been teleported to `{target_host}`. You used to be on `{source_host}`.**\n\
         >\n\
         > Teleport timestamp: {ts}. If anything looks off (missing context, broken paths, \
         stale cursor state, vanished tooling), this move is the most likely explanation. \
         The rest of this HANDOVER.md is what existed in your previous workdir at teleport time.\n\
         \n",
    )
}

/// Edit the canonical TOML in-place: set `[[agents]]` where name=agent
/// to host=target. Uses toml_edit to preserve formatting + comments.
fn update_toml_agent_host(config: &std::path::Path, agent: &str, target_host: &str) -> Result<()> {
    let original = std::fs::read_to_string(config)
        .with_context(|| format!("reading {}", config.display()))?;
    let mut doc: DocumentMut = original
        .parse()
        .with_context(|| format!("parsing {} as TOML", config.display()))?;
    let agents = doc
        .get_mut("agents")
        .and_then(|i| i.as_array_of_tables_mut())
        .ok_or_else(|| anyhow!("[[agents]] not found in TOML"))?;
    let mut updated = false;
    for entry in agents.iter_mut() {
        if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
            if name == agent {
                entry["host"] = value(target_host);
                updated = true;
                break;
            }
        }
    }
    if !updated {
        return Err(anyhow!(
            "agent `{agent}` not found in [[agents]] (TOML may have been edited concurrently)"
        ));
    }
    std::fs::write(config, doc.to_string())
        .with_context(|| format!("writing {}", config.display()))?;
    Ok(())
}

/// Run `giga sync --once` locally to push the updated TOML to peers.
fn sync_toml_to_peers(config: &std::path::Path) -> Result<()> {
    let status = Command::new(std::env::current_exe()?)
        .args([
            "sync",
            "--once",
            "--config",
            config.to_str().ok_or_else(|| anyhow!("non-UTF8 config path"))?,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("invoking giga sync --once")?;
    if !status.success() {
        return Err(anyhow!(
            "giga sync --once exited {}",
            status.code().unwrap_or(-1),
        ));
    }
    Ok(())
}

/// Run a giga subcommand on the target host via `giga remote`.
fn run_remote_giga(target: &Host, config: &std::path::Path, sub_args: &[&str]) -> Result<()> {
    let mut argv = vec![
        "remote".to_string(),
        "--host".to_string(),
        target.name.clone(),
        "--config".to_string(),
        config
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF8 config path"))?
            .to_string(),
        "--".to_string(),
    ];
    argv.extend(sub_args.iter().map(|s| s.to_string()));
    let status = Command::new(std::env::current_exe()?)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("invoking giga remote --host {}", target.name))?;
    if !status.success() {
        return Err(anyhow!(
            "giga remote on `{}` exited {}",
            target.name,
            status.code().unwrap_or(-1),
        ));
    }
    Ok(())
}

/// SSH the source host and gracefully kill the agent's old tmux
/// pane(s): SIGTERM (Ctrl+C) → 5s grace → kill-window.
///
/// v0.6.1 multi-runtime awareness: claude/agy agents have ONE pane
/// titled `<agent>`. Codex agents have TWO panes titled `<agent>-cli`
/// and `<agent>-bridge` (per the v0.6.0 launch.rs 2-pane layout). We
/// don't know the runtime here at kill time without the Config, so
/// fire kill-window at all three possible names — tmux returns "no
/// such window" silently for any that don't exist, so it's safe to
/// shotgun. The Ctrl+C is best-effort: only fires on existing windows.
fn kill_old_pane(source: &Host, project: &str, agent: &str) -> Result<()> {
    let ssh = build_ssh_target(source)?;
    let session = format!("giga-{project}");
    let cmd = format!(
        "tmux send-keys -t {session}:{agent}      C-c >/dev/null 2>&1 || true; \
         tmux send-keys -t {session}:{agent}-cli  C-c >/dev/null 2>&1 || true; \
         tmux send-keys -t {session}:{agent}-bridge C-c >/dev/null 2>&1 || true; \
         sleep 5; \
         tmux kill-window -t {session}:{agent}        >/dev/null 2>&1 || true; \
         tmux kill-window -t {session}:{agent}-cli    >/dev/null 2>&1 || true; \
         tmux kill-window -t {session}:{agent}-bridge >/dev/null 2>&1 || true",
    );
    sync::ssh_run(&ssh, &cmd).context("killing old pane on source")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// Two-host fixture loaded via Config::load (writes a real
    /// this_host.local.toml so multi-host validation passes).
    /// Returns (cfg, tmpdir) — caller must keep tmpdir alive.
    fn cfg_with_two_hosts() -> (Config, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(
            &cfg_path,
            r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"
ssh_user = "alice"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"
ssh_user = "bob"

[[agents]]
name = "research"
workdir = "/home/op/.giga/configs/t/workdirs/research"
role = "."
platform = "wsl"
host = "host-a"
"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("this_host.local.toml"),
            "this_host = \"host-a\"\n",
        )
        .unwrap();
        (Config::load(&cfg_path).unwrap(), tmp)
    }

    fn args_teleport_to(to: &str) -> Args {
        Args {
            agent: "research".into(),
            to: to.into(),
            from: None,
            keep_running: false,
            dry_run: true,
            config: PathBuf::from("/tmp/giga-harness-teleport-test.toml"),
        }
    }

    /// v0.5.0 T1: render_teleport_banner produces the canonical text.
    #[test]
    fn render_teleport_banner_includes_source_and_target() {
        let banner = render_teleport_banner("host-a", "host-b");
        assert!(banner.starts_with("> **You have been teleported to `host-b`"));
        assert!(banner.contains("You used to be on `host-a`"));
        assert!(banner.contains("Teleport timestamp:"));
        assert!(banner.contains("missing context, broken paths"));
        // Ends with a blank line so subsequent HANDOVER content is
        // visually separated.
        assert!(banner.ends_with("\n\n"));
    }

    /// v0.5.0 T4: preflight rejects unknown agent.
    #[test]
    fn preflight_rejects_unknown_agent() {
        let (cfg, _tmp) = cfg_with_two_hosts();
        let mut args = args_teleport_to("host-b");
        args.agent = "ghost".into();
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("ghost"));
        assert!(err.to_string().contains("not in [[agents]]"));
    }

    /// v0.5.0 T5: preflight rejects same source and target.
    #[test]
    fn preflight_rejects_same_source_and_target() {
        let (cfg, _tmp) = cfg_with_two_hosts();
        let mut args = args_teleport_to("host-a"); // research is already on host-a
        args.from = Some("host-a".into());
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("nothing to teleport"));
    }

    /// v0.5.0 T6: preflight rejects unknown target host.
    #[test]
    fn preflight_rejects_unknown_target_host() {
        let (cfg, _tmp) = cfg_with_two_hosts();
        let args = args_teleport_to("host-z");
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("host-z"));
        assert!(err.to_string().contains("not in [[hosts]]"));
    }

    /// v0.5.0 T7: preflight auto-detects source from agent.host when
    /// --from is omitted.
    #[test]
    fn preflight_auto_detects_source_from_toml() {
        let (cfg, _tmp) = cfg_with_two_hosts();
        let args = args_teleport_to("host-b");
        let plan = preflight(&cfg, &args).unwrap();
        assert_eq!(plan.source.name, "host-a");
        assert_eq!(plan.target.name, "host-b");
        assert_eq!(plan.agent, "research");
    }

    /// v0.5.0 T9: update_toml_agent_host changes the host field in
    /// the canonical TOML and leaves the rest of the doc intact.
    #[test]
    fn update_toml_agent_host_changes_host_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("giga-harness.toml");
        std::fs::write(
            &path,
            r#"# top comment preserved
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "b.tail0.ts.net"

[[agents]]
name = "research"
workdir = "/h/research"
role = "."
platform = "wsl"
host = "host-a"
"#,
        )
        .unwrap();

        update_toml_agent_host(&path, "research", "host-b").unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("# top comment preserved"), "comments must survive edit");
        assert!(body.contains("host = \"host-b\""), "host should be flipped");
        assert!(!body.contains("host = \"host-a\""), "old host should be gone");

        // Re-parse to make sure it's still a valid TOML.
        let _: DocumentMut = body.parse().unwrap();
    }

    /// v0.5.0 T9b: update_toml_agent_host errors when agent missing.
    #[test]
    fn update_toml_agent_host_errors_when_agent_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("giga-harness.toml");
        std::fs::write(
            &path,
            r#"
[project]
name = "t"
[paths]
wsl_inbox = "/tmp/i"
[[hosts]]
name = "host-a"
tailnet_hostname = "a.tail0.ts.net"
[[agents]]
name = "alice"
workdir = "/h/a"
role = "."
platform = "wsl"
host = "host-a"
"#,
        )
        .unwrap();
        let err = update_toml_agent_host(&path, "ghost", "host-a").unwrap_err();
        assert!(err.to_string().contains("ghost"));
        assert!(err.to_string().contains("not found"));
    }
}
