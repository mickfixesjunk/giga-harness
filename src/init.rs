//! `giga init` — scaffold inbox files and per-agent AGENTS.md from a config.
//!
//! Idempotent: re-running against an existing config is safe. Inbox
//! files that already exist keep their content (only the header gets
//! re-written if missing). AGENTS.md files are always re-rendered
//! from the template so config changes propagate.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{Agent, Config};
use crate::fs_paths::to_host_fs;
use crate::trust;

pub fn run(config_path: &Path) -> Result<()> {
    run_with(config_path, true)
}

pub fn run_with(config_path: &Path, do_trust: bool) -> Result<()> {
    let cfg = Config::load(config_path)?;
    // v0.6.4 fix: derive config_dir from the CANONICALIZED path so
    // claudemd_template relative paths resolve against the swarm dir,
    // NOT a workdir-side symlink to the canonical config. Same class
    // of bug as v0.3.7 Bug 1 (this_host.toml symlink leakage) — fixed
    // there but missed for template lookup. Symptom: `giga launch
    // --only X` from a workdir/<agent>/ cwd errored with "No such file
    // or directory" on `agents/<other-agent>.md` because the parent
    // dir of the symlink was the workdir, not the swarm dir.
    let abs_config = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.to_path_buf());
    let config_dir = abs_config
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent dir"))?;

    // Host-aware filtering: when this_host is set (cross-host swarm), only
    // scaffold local-host artifacts — agents whose host matches this_host,
    // and channels with at least one participant on this_host. Without
    // this we'd try to mkdir + write AGENTS.md to agent workdirs that
    // belong on a different physical machine (e.g. /home/neo/... when
    // we're on a box with user `neomatrix`). For legacy local-only
    // swarms (no [[hosts]], no this_host), include everything — today's
    // behavior, unchanged.
    let local_agents: Vec<&Agent> = if cfg.this_host.is_some() {
        cfg.agents
            .iter()
            .filter(|a| cfg.agent_host(a) == cfg.this_host.as_deref())
            .collect()
    } else {
        cfg.agents.iter().collect()
    };
    // v0.3.9 Bug 5: name the agents we're NOT scaffolding so the
    // success message reflects reality. Pre-fix: init exited "OK — 4
    // agent AGENTS.md files in place" without saying it had skipped
    // 3 others that live on a peer host.
    let skipped_agents: Vec<&Agent> = if let Some(this) = cfg.this_host.as_deref() {
        cfg.agents
            .iter()
            .filter(|a| cfg.agent_host(a).map(|h| h != this).unwrap_or(false))
            .collect()
    } else {
        Vec::new()
    };
    let local_channels: Vec<&crate::config::Channel> = if let Some(this) = cfg.this_host.as_deref() {
        cfg.channels
            .iter()
            .filter(|c| {
                c.participants.iter().any(|p| {
                    cfg.agents
                        .iter()
                        .find(|a| a.name == *p)
                        .and_then(|a| cfg.agent_host(a))
                        .map(|h| h == this)
                        .unwrap_or(false)
                })
            })
            .collect()
    } else {
        cfg.channels.iter().collect()
    };

    println!("project: {}", cfg.project.name);
    if cfg.this_host.is_some() {
        println!(
            "agents:  {} ({} local on `{}`)",
            cfg.agents.len(),
            local_agents.len(),
            cfg.this_host.as_deref().unwrap_or("?"),
        );
        println!(
            "channels:{} ({} local on `{}`)",
            cfg.channels.len(),
            local_channels.len(),
            cfg.this_host.as_deref().unwrap_or("?"),
        );
    } else {
        println!("agents:  {}", cfg.agents.len());
        println!("channels:{}", cfg.channels.len());
    }

    // Ensure inbox dirs exist. v0.3.2+: respects the per-host [paths]
    // override on [[hosts]] entries so a peer with asymmetric paths
    // (different $HOME, different Windows user) doesn't try to mkdir
    // the operator's literal path.
    //
    // v0.3.4+ (quality F9): only scaffold paths whose channels are
    // actually local to this host. Before this, a wsl-only peer would
    // try to mkdir `windows_inbox` (e.g. /mnt/c/Users/.../something)
    // even though no local agents have side=windows channels. On
    // morpheus-wsl this manifested as init failing on a Windows path
    // belonging to a different user on the operator's box. For the
    // legacy local-only case (no this_host, no [[hosts]]) all sides
    // are still in scope — preserves today's behavior.
    let this_host = cfg.this_host.as_deref();
    let need_wsl = local_channels.iter().any(|c| c.side == "wsl");
    let need_windows = local_channels.iter().any(|c| c.side == "windows");
    if need_wsl {
        if let Some(p) = cfg.inbox_for_host_side(this_host, "wsl") {
            fs::create_dir_all(&p).with_context(|| format!("mkdir -p {}", p.display()))?;
        }
    }
    if need_windows {
        if let Some(p) = cfg.inbox_for_host_side(this_host, "windows") {
            fs::create_dir_all(&p).with_context(|| format!("mkdir -p {}", p.display()))?;
        }
    }

    // Create channel files with convention headers if absent.
    for ch in &local_channels {
        let path = cfg.channel_path(ch)?;
        if path.exists() && path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            println!("  [keep] {}", path.display());
            continue;
        }
        let header = render_channel_header(&cfg, ch);
        fs::write(&path, header).with_context(|| format!("write {}", path.display()))?;
        println!("  [new]  {}", path.display());
    }

    // v0.3.9 Bug 5: explicit visibility on what's being skipped.
    for agent in &skipped_agents {
        let host = cfg.agent_host(agent).unwrap_or("?");
        println!("  [skip] {} (lives on `{host}`, not this host)", agent.name);
    }

    // Generate per-agent AGENTS.md in the agent's workdir. The
    // workdir comes from the config in its agent-side form (e.g.,
    // `C:\Users\Audio\sdd-testwin` for Windows-platform agents on a
    // Linux/WSL host); translate to a host-FS path before touching
    // the filesystem so we don't end up with literal-backslash dirs.
    //
    // Also: if the agent has an AGENTS.md template at
    // `agents/<name>.md`, look for an optional handover file at
    // `agents/<name>.handover.md` next to it. When present, copy
    // it into the workdir as `HANDOVER.md` on first init only —
    // preserving any session appends the agent has accumulated in
    // its workdir copy. The config dir's template is the round-trip
    // checkpoint; the workdir copy is the agent's live append log.
    for agent in &local_agents {
        let host_workdir = to_host_fs(&agent.workdir);
        fs::create_dir_all(&host_workdir)
            .with_context(|| format!("mkdir -p agent workdir {}", host_workdir.display()))?;
        // v0.6.0: universal AGENTS.md filename across runtimes. Modern
        // Claude Code reads AGENTS.md alongside CLAUDE.md; codex + agy
        // expect AGENTS.md natively. Single source of truth.
        let agents_md_path = host_workdir.join("AGENTS.md");
        let body = render_agent_claudemd(&cfg, agent, config_dir, &abs_config)?;
        fs::write(&agents_md_path, body)
            .with_context(|| format!("write {}", agents_md_path.display()))?;
        println!("  [gen]  {}", agents_md_path.display());

        // v0.6.7: removed the v0.6.0 belt-and-suspenders
        // CLAUDE.md → AGENTS.md symlink. Modern Claude Code reads
        // AGENTS.md natively; the symlink was for legacy versions
        // that's a non-issue now. Single source of truth: AGENTS.md.
        // Existing CLAUDE.md files in workdirs are left untouched —
        // operator cleanup script (one-liner) handles old swarms.

        // v0.6.0: for codex-runtime agents, scaffold the channel-bridge
        // directory tree under the agent's workdir. The codex CLI reads
        // CODEX_CHANNEL_DIR=<workdir>/codex-channel; the bridge (giga
        // watch --codex) writes envelopes into inbox/ and reads receipts
        // from outbox/.
        if cfg.agent_runtime(agent) == crate::runtime::Runtime::Codex {
            let bridge_dir = host_workdir.join("codex-channel");
            for sub in ["inbox", "outbox", "processed"] {
                let p = bridge_dir.join(sub);
                fs::create_dir_all(&p)
                    .with_context(|| format!("mkdir -p {}", p.display()))?;
            }
            println!("  [codex] {} (inbox/outbox/processed)", bridge_dir.display());
        }

        // Symlink the project config into the workdir so the agent's
        // bare `giga watch --as <name>` (whose --config defaults to
        // `giga-harness.toml` in cwd) resolves without an explicit
        // --config. Unix/WSL-side agents only: a unix symlink to a
        // /home path is meaningless to a Windows-native agent. Idempotent —
        // only created when nothing is already at that path.
        #[cfg(unix)]
        if agent.platform != "windows" {
            let link = host_workdir.join("giga-harness.toml");
            if link.symlink_metadata().is_err() {
                match std::os::unix::fs::symlink(&abs_config, &link) {
                    Ok(()) => println!("  [link] {}", link.display()),
                    Err(e) => eprintln!(
                        "  [link] warning: couldn't symlink config into {} — {}",
                        host_workdir.display(),
                        e,
                    ),
                }
            }
        }

        if let Some(tpl) = &agent.claudemd_template {
            let handover_rel = handover_template_for(tpl);
            let handover_abs = if handover_rel.is_absolute() {
                handover_rel
            } else {
                config_dir.join(handover_rel)
            };
            if handover_abs.exists() {
                let dest = host_workdir.join("HANDOVER.md");
                if dest.exists() {
                    println!(
                        "  [keep] {} (workdir copy preserved — agent's session appends)",
                        dest.display(),
                    );
                } else {
                    fs::copy(&handover_abs, &dest).with_context(|| {
                        format!(
                            "copy handover {} → {}",
                            handover_abs.display(),
                            dest.display(),
                        )
                    })?;
                    println!("  [hand] {}", dest.display());
                }
            }
        }
    }

    if do_trust {
        match trust::pre_trust(&cfg) {
            Ok(n) => println!(
                "  [trust] marked {} agent workdir(s) as trusted in Claude Code",
                n
            ),
            Err(e) => eprintln!("  [trust] warning: couldn't pre-populate trust — {}", e),
        }
    }

    // Upsert this swarm into the cross-swarm registry so the user can
    // resume from anywhere under any agent's code_root just by typing
    // `giga launch` — no need to `cd` to the config dir. (`abs_config`
    // was resolved up top.)
    let mut code_roots: Vec<std::path::PathBuf> = cfg
        .agents
        .iter()
        .filter_map(|a| a.code_root.clone())
        .collect();
    code_roots.sort();
    code_roots.dedup();
    match crate::registry::upsert(&cfg.project.name, &abs_config, &code_roots) {
        Ok(true) => println!(
            "  [reg]  swarm `{}` registered → {}",
            cfg.project.name,
            abs_config.display()
        ),
        Ok(false) => {}
        Err(e) => eprintln!("  [reg] warning: couldn't update swarm registry — {}", e),
    }

    if skipped_agents.is_empty() {
        println!(
            "\nginit OK — {} channels + {} agent AGENTS.md files in place",
            local_channels.len(),
            local_agents.len(),
        );
    } else {
        println!(
            "\nginit OK — {} channels + {} local agent AGENTS.md files in place; {} skipped (live on other hosts)",
            local_channels.len(),
            local_agents.len(),
            skipped_agents.len(),
        );
    }
    println!("next: `giga launch <config>` to open the terminals");
    Ok(())
}

/// Given an agent's AGENTS.md template path (e.g.,
/// `agents/superdeduper.md`), return the sibling handover path
/// (`agents/superdeduper.handover.md`). The file may or may not
/// exist; the caller checks before copying.
fn handover_template_for(claudemd: &Path) -> PathBuf {
    let stem = claudemd
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = claudemd.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{stem}.handover.md"))
}

fn render_channel_header(cfg: &Config, ch: &crate::config::Channel) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# {} shared inbox\n\nProject: {}\n",
        ch.participants.join(" ↔ "),
        cfg.project.name,
    ));
    if let Some(purpose) = &ch.purpose {
        s.push_str(&format!("Purpose: {purpose}\n"));
    }
    s.push_str(
        "\n## Convention\n\n\
         Append-only. Each message gets a header block:\n\n\
         ```\n\
         ===\n\
         [<sender>] <subject> — <UTC ISO-8601 timestamp>\n\
         ===\n\n\
         <body>\n\n\
         WAITING ON: <agent-name> (<what's needed>)   ← OR\n\
         (Informational, no response required.)\n\
         ===\n\
         ```\n\n\
         Read the full file before replying. Don't edit anyone else's\n\
         messages — post corrections as new messages.\n\n",
    );
    s.push_str("## Self-arm your watcher (do this once per session)\n\n");
    s.push_str(
        "    Monitor(\n      description: \"giga inbox watcher\",\n      persistent: true,\n      command: \"giga watch --as <your-name>\"\n    )\n\n",
    );
    s.push_str("Replace `<your-name>` with whichever participant you are.\n");
    s.push_str("This single watcher tracks every channel you participate in via giga-harness.toml — not just this one. New channels added later are picked up automatically (~15s).\n");
    s.push_str("Stop with TaskStop when you no longer want events.\n");
    s
}

pub(crate) fn render_agent_claudemd(
    cfg: &Config,
    agent: &Agent,
    config_dir: &Path,
    config_path: &Path,
) -> Result<String> {
    // If the agent has an explicit template, use it verbatim (the
    // template author owns the content). Otherwise auto-generate.
    if let Some(tpl) = &agent.claudemd_template {
        let abs = if tpl.is_absolute() {
            tpl.clone()
        } else {
            config_dir.join(tpl)
        };
        let body = fs::read_to_string(&abs)
            .with_context(|| format!("reading agent AGENTS.md template {}", abs.display()))?;
        return Ok(prepend_header(&body, agent, cfg, config_path));
    }

    // Auto-generated minimal AGENTS.md.
    let mut s = String::new();
    s.push_str(&format!("# {} agent\n\n", agent.name));
    s.push_str(&format!("**Role:** {}\n\n", agent.role));
    s.push_str(&format!(
        "**Working directory:** `{}`\n\n",
        agent.workdir.display()
    ));
    s.push_str(&format!(
        "## Project pipeline\n\n_(from {} config)_\n\n",
        cfg.project.name
    ));

    // Channels this agent watches — auto-discovered at runtime by a
    // single config-aware watcher.
    let mine: Vec<&crate::config::Channel> = cfg
        .channels
        .iter()
        .filter(|ch| ch.participants.iter().any(|p| p == &agent.name))
        .collect();
    if !mine.is_empty() {
        let channel_list = mine
            .iter()
            .map(|c| format!("`{}`", c.file))
            .collect::<Vec<_>>()
            .join(", ");
        // v0.6.0: per-runtime Session Start snippet. The Session Start
        // header itself is already inside the bundled snippet — don't
        // duplicate it. Each runtime's snippet lives in
        // templates/runtimes/<runtime>.md and is bundled at compile time.
        let runtime = cfg.agent_runtime(agent);
        s.push_str(
            &runtime
                .session_start_snippet()
                .replace("{{AGENT}}", &agent.name),
        );
        s.push_str(&format!(
            "\nYour channels: {channel_list}.\n\n",
        ));
    }

    // Bench-coord pointer.
    if let Some(bp) = &cfg.bench_protocol {
        if agent.bench_scheduler {
            s.push_str("## Bench coordination\n\nYou are the bench scheduler. ");
        } else {
            s.push_str("## Bench coordination\n\n");
        }
        s.push_str(&format!(
            "Slot pool: {}. Scheduler: `{}`. Before any CPU/IO-heavy work, post `bench-request` on the channel with the scheduler and wait for `bench-clear`. Standing-clearance — release with `bench-done`.\n\n",
            bp.slot_pool, bp.scheduler,
        ));
    }

    s.push_str("## Convention\n\n");
    s.push_str(crate::templates::CONVENTION.trim_end());
    s.push_str("\n\n");

    Ok(prepend_header(&s, agent, cfg, config_path))
}

/// v0.3.6: render the "Swarm coordination" section that goes into the
/// AGENTS.md of an agent with `swarm_boss = true`. The agent's session
/// arms sync + merger Monitors instead of the operator spawning tmux
/// daemon panes. See SWARM_BOSS_DESIGN.md §3.3.
fn render_swarm_boss_section(host: &str, config_path: &Path) -> String {
    let cfg_str = config_path.display();
    format!(
        "## Swarm coordination (this agent is the swarm_boss for `{host}`)\n\n\
         In addition to your inbox watcher, arm two coordination daemons \
         as Monitors. These keep cross-host channel comms flowing for every \
         agent on this host:\n\n\
         ```\n\
         Monitor(\n\
         \u{20}\u{20}description: \"giga sync — push slices to peers\",\n\
         \u{20}\u{20}persistent: true,\n\
         \u{20}\u{20}command: \"giga sync --quiet --config {cfg_str}\"\n\
         )\n\
         \n\
         Monitor(\n\
         \u{20}\u{20}description: \"giga merger — pull peer slices into local merged files\",\n\
         \u{20}\u{20}persistent: true,\n\
         \u{20}\u{20}command: \"giga merger --quiet --config {cfg_str}\"\n\
         )\n\
         ```\n\n\
         These are `--quiet` mode daemons — they emit lines only on \
         errors or state changes. Most notifications you receive from \
         them will be real signals worth surfacing or acting on.\n\n\
         If either Monitor stops firing for >5 minutes during active \
         swarm work, restart it (TaskStop then re-arm). Daemons dying \
         mid-session silently disrupts cross-host visibility for THIS \
         host until restarted.\n\n",
    )
}

fn prepend_header(body: &str, agent: &Agent, cfg: &Config, config_path: &Path) -> String {
    let mut out = format!(
        "<!--\n  Generated by giga-harness from this swarm's agent template\n  (its `claudemd_template`, or a built-in default if none is set).\n  Edits to THIS workdir copy are overwritten on the next `giga init`\n  or `giga launch` — to persist changes, edit the source template.\n  Agent: {}\n-->\n\n",
        agent.name,
    );
    out.push_str(&format!(
        "> **You are the `{slug}` agent.** Every response you make to the user in \
         this terminal MUST start with `[{slug}]` so the user can tell at a glance \
         which agent is talking. This applies to every assistant turn, not just \
         channel messages.\n\n",
        slug = agent.name,
    ));
    if let Some(cr) = &agent.code_root {
        out.push_str(&format!(
            "> **Code root:** `{}` \\\n> All code work (edits, builds, tests) happens here. `cd` to this directory before touching project files. Your workdir (`{}`) is only your launch context and AGENTS.md home.\n\n",
            cr.display(),
            agent.workdir.display(),
        ));
    }
    // v0.3.6: swarm_boss agents arm sync + merger Monitors at session
    // start. Inject the coordination section so a fresh init carries
    // the instructions regardless of whether the agent uses a custom
    // claudemd_template or the auto-generated one.
    //
    // v0.3.7 Bug 10 fix: gate on [[hosts]] non-empty. In a local-only
    // swarm, sync/merger exit immediately ("no [[hosts]] declared")
    // and Monitor reports the task as completed — looks like a daemon
    // crash. Keep the flag legal in a local-only TOML (so users can
    // set it up ahead of adding hosts) but don't inject Monitor lines
    // that would only fire once and look broken. Re-run `giga init`
    // after adding the first host to materialize them.
    if agent.swarm_boss && !cfg.hosts.is_empty() {
        let host = agent
            .host
            .as_deref()
            .or(cfg.this_host.as_deref())
            .unwrap_or("this-host");
        out.push_str(&render_swarm_boss_section(host, config_path));
    }
    out.push_str(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn config_with_one_agent(code_root: Option<&str>) -> Config {
        let cr_line = code_root
            .map(|p| format!("code_root = \"{p}\"\n"))
            .unwrap_or_default();
        let body = format!(
            r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "design"
workdir = "/h/design"
{cr_line}role = "scope owner"
platform = "wsl"
"#,
        );
        Config::load_str_for_test(&body).unwrap()
    }

    #[test]
    fn claudemd_always_contains_identity_callout() {
        let cfg = config_with_one_agent(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let body = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path(), &tmp.path().join("giga-harness.toml")).unwrap();
        assert!(
            body.contains("You are the `design` agent"),
            "identity callout missing — agent won't self-identify in replies"
        );
        assert!(
            body.contains("[design]"),
            "reply-prefix instruction missing — agent won't prefix its replies"
        );
    }

    #[test]
    fn claudemd_contains_code_root_callout_when_set() {
        let cfg = config_with_one_agent(Some("/code/myproj"));
        let tmp = tempfile::TempDir::new().unwrap();
        let body = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path(), &tmp.path().join("giga-harness.toml")).unwrap();
        assert!(
            body.contains("Code root:") && body.contains("/code/myproj"),
            "code_root callout missing or path wrong:\n{}",
            body,
        );
    }

    #[test]
    fn claudemd_omits_code_root_callout_when_unset() {
        let cfg = config_with_one_agent(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let body = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path(), &tmp.path().join("giga-harness.toml")).unwrap();
        assert!(
            !body.contains("Code root:"),
            "code_root callout should not appear when field is unset",
        );
    }

    #[test]
    fn claudemd_preserves_template_body_under_callout_header() {
        // When the agent has a custom template, prepend_header injects
        // the callouts at the top but must preserve the template body
        // verbatim below them. (Templates are user-authored and should
        // never be silently modified.)
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        let tpl_path = agents_dir.join("design.md");
        let tpl_body = "# my custom template\n\nCustom body content the user wrote.\n";
        fs::write(&tpl_path, tpl_body).unwrap();

        let cfg_text = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "design"
workdir = "/h/design"
role = "."
platform = "wsl"
claudemd_template = "agents/design.md"
"#;
        let cfg = Config::load_str_for_test(cfg_text).unwrap();
        let body = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path(), &tmp.path().join("giga-harness.toml")).unwrap();
        assert!(body.contains(tpl_body), "custom template body was modified");
        assert!(
            body.contains("You are the `design` agent"),
            "identity callout still injected for custom templates",
        );
    }

    /// v0.3.4 fix for quality finding 9: a wsl-only peer must NOT try
    /// to mkdir the global `paths.windows_inbox` when no local channel
    /// has `side = "windows"`. Pre-fix: init scaffolded BOTH wsl and
    /// windows inbox dirs unconditionally if either was set in [paths].
    /// Repro: morpheus-wsl had a windows_inbox path pointing at the
    /// operator's box (different Windows user); init failed mkdir.
    #[test]
    fn init_skips_windows_inbox_when_no_local_windows_channel() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wsl_inbox = tmp.path().join("wsl-inbox");
        // Path inside tmp that does NOT exist yet; init will mkdir if
        // it visits it. Test passes when it's still missing afterward.
        let windows_inbox = tmp.path().join("nonexistent-windows-inbox");
        let cfg_text = format!(
            r#"
[project]
name = "t"

[paths]
wsl_inbox = '{wsl}'
windows_inbox = '{win}'

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"

[[agents]]
name = "alice"
workdir = '{workdir_alice}'
role = "."
platform = "wsl"
host = "host-a"

[[agents]]
name = "bob"
workdir = '{workdir_bob}'
role = "."
platform = "wsl"
host = "host-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            wsl = wsl_inbox.display(),
            win = windows_inbox.display(),
            workdir_alice = tmp.path().join("alice-wd").display(),
            workdir_bob = tmp.path().join("bob-wd").display(),
        );
        let config_path = tmp.path().join("giga-harness.toml");
        fs::write(&config_path, cfg_text).unwrap();
        fs::write(tmp.path().join("this_host.toml"), "this_host = \"host-a\"\n").unwrap();

        run_with(&config_path, false).unwrap();

        assert!(wsl_inbox.exists(), "wsl_inbox should be created (local wsl channel)");
        assert!(
            !windows_inbox.exists(),
            "windows_inbox should NOT be created on a wsl-only peer; quality F9"
        );
    }

    #[test]
    fn claudemd_lists_channels_the_agent_participates_in() {
        // Auto-generated AGENTS.md (no template) lists the agent's
        // channels so the watcher arming command is self-documenting.
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "design"
workdir = "/h/design"
role = "."
platform = "wsl"

[[agents]]
name = "code"
workdir = "/h/code"
role = "."
platform = "wsl"

[[channels]]
file = "code-design.md"
side = "wsl"
participants = ["code", "design"]
"#;
        let cfg = Config::load_str_for_test(body).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let claudemd = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path(), &tmp.path().join("giga-harness.toml")).unwrap();
        assert!(claudemd.contains("code-design.md"));
        assert!(claudemd.contains("giga watch --as design"));
    }

    /// v0.3.6 S3: when an agent is flagged swarm_boss, its AGENTS.md
    /// includes the Swarm coordination section with sync + merger
    /// Monitor lines. The agent will arm them at session start.
    #[test]
    fn claudemd_includes_swarm_coordination_section_when_swarm_boss() {
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[agents]]
name = "design"
workdir = "/h/design"
role = "scope owner"
platform = "wsl"
host = "host-a"
swarm_boss = true
"#;
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        fs::write(&cfg_path, body).unwrap();
        fs::write(tmp.path().join("this_host.toml"), "this_host = \"host-a\"\n").unwrap();
        let cfg = Config::load(&cfg_path).unwrap();

        let claudemd = render_agent_claudemd(&cfg, &cfg.agents[0], tmp.path(), &cfg_path).unwrap();
        assert!(
            claudemd.contains("Swarm coordination"),
            "swarm_boss section missing from AGENTS.md"
        );
        assert!(
            claudemd.contains("giga sync --quiet"),
            "sync Monitor command missing"
        );
        assert!(
            claudemd.contains("giga merger --quiet"),
            "merger Monitor command missing"
        );
        assert!(
            claudemd.contains(&cfg_path.display().to_string()),
            "Monitor command must include the absolute config path"
        );
        assert!(
            claudemd.contains("host-a"),
            "section should name the host the boss is responsible for"
        );
    }

    /// v0.3.9 Bug 5 visibility: when init runs on a host where some
    /// agents live elsewhere, the skipped agents must be enumerated
    /// (otherwise the success message looks like everything worked
    /// while peer-hosted workdirs are silently missing).
    #[test]
    fn init_skips_agents_on_other_hosts_and_skip_count_in_summary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wsl_inbox = tmp.path().join("wsl-inbox");
        let cfg_text = format!(
            r#"
[project]
name = "t"

[paths]
wsl_inbox = '{wsl}'

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"

[[agents]]
name = "alice"
workdir = '{workdir_alice}'
role = "."
platform = "wsl"
host = "host-a"

[[agents]]
name = "bob"
workdir = '{workdir_bob}'
role = "."
platform = "wsl"
host = "host-b"
"#,
            wsl = wsl_inbox.display(),
            workdir_alice = tmp.path().join("alice-wd").display(),
            workdir_bob = tmp.path().join("bob-wd").display(),
        );
        let config_path = tmp.path().join("giga-harness.toml");
        fs::write(&config_path, cfg_text).unwrap();
        fs::write(tmp.path().join("this_host.local.toml"), "this_host = \"host-a\"\n").unwrap();

        run_with(&config_path, false).unwrap();

        // alice's workdir created; bob's was skipped (lives on host-b).
        // v0.6.7: AGENTS.md is the universal filename; CLAUDE.md is no
        // longer auto-symlinked.
        assert!(tmp.path().join("alice-wd").join("AGENTS.md").exists());
        assert!(
            !tmp.path().join("bob-wd").exists(),
            "bob's workdir must NOT be created on host-a (lives on host-b)"
        );
    }

    /// v0.3.7 Bug 10 fix: swarm_boss flag set on a local-only swarm
    /// (no [[hosts]] yet) does NOT inject sync/merger Monitor lines.
    /// Those daemons exit immediately on a local-only config, so
    /// Monitor reports them as completed/crashed — confusing UX.
    #[test]
    fn claudemd_omits_swarm_coordination_section_when_local_only_swarm() {
        let cfg = Config::load_str_for_test(
            r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "design"
workdir = "/h/design"
role = "."
platform = "wsl"
swarm_boss = true
"#,
        )
        .unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let body = render_agent_claudemd(
            &cfg,
            &cfg.agents[0],
            tmp.path(),
            &tmp.path().join("giga-harness.toml"),
        )
        .unwrap();
        assert!(
            !body.contains("Swarm coordination"),
            "swarm_boss on a local-only swarm must not inject sync/merger Monitors"
        );
    }

    /// v0.3.6 S4: agents without swarm_boss flag get no coordination
    /// section. Default-off invariant.
    #[test]
    fn claudemd_omits_swarm_coordination_section_when_not_swarm_boss() {
        let cfg = config_with_one_agent(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let body = render_agent_claudemd(
            &cfg,
            &cfg.agents[0],
            tmp.path(),
            &tmp.path().join("giga-harness.toml"),
        )
        .unwrap();
        assert!(!body.contains("Swarm coordination"));
        assert!(!body.contains("giga sync --quiet"));
        assert!(!body.contains("giga merger --quiet"));
    }
}
