//! Pure AGENTS.md / channel-header text generation for `giga init`.
//!
//! Everything here is side-effect free: it consumes a [`Config`] +
//! [`Agent`] / [`Channel`] and returns a `String`. `init.rs` owns the
//! effects (mkdir, file writes, symlink, trust, registry upsert) and
//! calls into this module to produce the bytes it writes; `takeover.rs`
//! reuses [`render_agent_claudemd`] to re-render AGENTS.md on a runtime
//! flip. Keeping the rendering separate makes the byte-for-byte output
//! testable without touching the filesystem.

use std::path::Path;

use anyhow::{Context, Result};

use crate::config::{Agent, Config};

pub(crate) fn render_channel_header(cfg: &Config, ch: &crate::config::Channel) -> String {
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
    // If the agent has an explicit template, the template author owns
    // the role prose — but the Session Start section is still rendered
    // per-runtime. A `{{SESSION_START}}` placeholder (what `giga setup`
    // writes) — or, for pre-placeholder templates, an existing
    // `## Session Start` section — is replaced with the runtime's
    // watcher-arming snippet so a `runtime = "agy"` / "codex" agent gets
    // the correct protocol without hand-editing the template. No flag
    // needed: the runtime is read straight from the TOML.
    if let Some(tpl) = &agent.claudemd_template {
        let abs = if tpl.is_absolute() {
            tpl.clone()
        } else {
            config_dir.join(tpl)
        };
        let body = std::fs::read_to_string(&abs)
            .with_context(|| format!("reading agent AGENTS.md template {}", abs.display()))?;
        let snippet = cfg
            .agent_runtime(agent)
            .session_start_snippet()
            .replace("{{AGENT}}", &agent.name);
        let body = inject_session_start(&body, &snippet);
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
        s.push_str(&format!("\nYour channels: {channel_list}.\n\n",));
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
    s.push_str(crate::scaffold::templates::CONVENTION.trim_end());
    s.push_str("\n\n");

    Ok(prepend_header(&s, agent, cfg, config_path))
}

/// Inject the runtime-specific Session Start snippet into a custom
/// AGENTS.md template. Called only for agents with a `claudemd_template`
/// (the auto-generated path already picks the right snippet directly).
///
/// Priority:
///   1. `{{SESSION_START}}` placeholder (what `giga setup` writes into
///      runtime-agnostic templates) — every occurrence is replaced with
///      `snippet`.
///   2. Legacy fallback: a line-anchored `## Session Start...` heading.
///      The whole section (that heading through the line before the next
///      `## ` heading, or EOF) is replaced. This lets pre-placeholder
///      swarms become runtime-correct on the next `giga init` without
///      anyone editing the template by hand.
///   3. Neither present: the body is returned unchanged.
///
/// `snippet` must already have `{{AGENT}}` substituted; it begins with
/// its own `## Session Start` heading.
fn inject_session_start(body: &str, snippet: &str) -> String {
    const PLACEHOLDER: &str = "{{SESSION_START}}";
    let snippet = snippet.trim_end();
    if body.contains(PLACEHOLDER) {
        return body.replace(PLACEHOLDER, snippet);
    }

    let lines: Vec<&str> = body.lines().collect();
    let is_session_start_heading = |l: &str| {
        let t = l.trim_start();
        t.starts_with("## ")
            && t.trim_start_matches('#')
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("session start")
    };
    let Some(start) = lines.iter().position(|l| is_session_start_heading(l)) else {
        return body.to_string();
    };
    // End = next top-level (`## `) heading after the Session Start one,
    // or EOF. The runtime snippet may itself contain `## ` subsections,
    // but we only look at the ORIGINAL body here so that's irrelevant.
    let end = ((start + 1)..lines.len())
        .find(|&i| lines[i].trim_start().starts_with("## "))
        .unwrap_or(lines.len());

    let mut result = lines[..start].join("\n");
    result.truncate(result.trim_end().len());
    if !result.is_empty() {
        result.push_str("\n\n");
    }
    result.push_str(snippet);
    if end < lines.len() {
        result.push_str("\n\n");
        result.push_str(&lines[end..].join("\n"));
    }
    if body.ends_with('\n') {
        result.push('\n');
    }
    result
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
    use std::fs;

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
        let body = render_agent_claudemd(
            &cfg,
            &cfg.agents[0],
            tmp.path(),
            &tmp.path().join("giga-harness.toml"),
        )
        .unwrap();
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
        let body = render_agent_claudemd(
            &cfg,
            &cfg.agents[0],
            tmp.path(),
            &tmp.path().join("giga-harness.toml"),
        )
        .unwrap();
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
        let body = render_agent_claudemd(
            &cfg,
            &cfg.agents[0],
            tmp.path(),
            &tmp.path().join("giga-harness.toml"),
        )
        .unwrap();
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
        let body = render_agent_claudemd(
            &cfg,
            &cfg.agents[0],
            tmp.path(),
            &tmp.path().join("giga-harness.toml"),
        )
        .unwrap();
        assert!(body.contains(tpl_body), "custom template body was modified");
        assert!(
            body.contains("You are the `design` agent"),
            "identity callout still injected for custom templates",
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
        let claudemd = render_agent_claudemd(
            &cfg,
            &cfg.agents[0],
            tmp.path(),
            &tmp.path().join("giga-harness.toml"),
        )
        .unwrap();
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
        fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"host-a\"\n",
        )
        .unwrap();
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

    // ---- inject_session_start ----

    #[test]
    fn inject_replaces_placeholder() {
        let body = "# alice\n\nrole stuff.\n\n{{SESSION_START}}\n\n## Convention\n\nx\n";
        let out = inject_session_start(body, "## Session Start\n\nARMED.\n");
        assert!(out.contains("ARMED."));
        assert!(!out.contains("{{SESSION_START}}"));
        // surrounding content preserved
        assert!(out.contains("role stuff."));
        assert!(out.contains("## Convention"));
    }

    #[test]
    fn inject_replaces_legacy_section_midbody() {
        // No placeholder — a hand-written `## Session Start` section
        // between two others must be swapped out, neighbors preserved.
        let body = "## Channels\n\n- a.md\n\n## Session Start\n\n1. Arm the Monitor.\n2. Standby.\n\n## Convention\n\nWAITING ON tag.\n";
        let out = inject_session_start(
            body,
            "## Session Start (do this first)\n\nRUNTIME SNIPPET.\n",
        );
        assert!(
            out.contains("RUNTIME SNIPPET."),
            "snippet not injected:\n{out}"
        );
        assert!(
            !out.contains("Arm the Monitor."),
            "old section survived:\n{out}"
        );
        assert!(
            out.contains("## Channels") && out.contains("- a.md"),
            "preceding section lost"
        );
        assert!(
            out.contains("## Convention") && out.contains("WAITING ON tag."),
            "following section lost"
        );
        // exactly one Session Start heading remains
        assert_eq!(out.matches("Session Start").count(), 1);
    }

    #[test]
    fn inject_replaces_legacy_section_at_eof() {
        let body = "## Role\n\nstuff.\n\n## Session Start\n\nold monitor text.\n";
        let out = inject_session_start(body, "## Session Start\n\nNEW.\n");
        assert!(out.contains("NEW."));
        assert!(!out.contains("old monitor text."));
        assert!(out.contains("## Role") && out.contains("stuff."));
    }

    #[test]
    fn inject_noop_when_no_session_start_or_placeholder() {
        let body = "# custom\n\njust role prose, no session start.\n";
        let out = inject_session_start(body, "## Session Start\n\nSNIP.\n");
        assert_eq!(out, body, "body with no Session Start must be untouched");
    }

    #[test]
    fn custom_template_agy_agent_gets_agy_session_start() {
        // End-to-end: a custom template with a legacy Monitor Session
        // Start, an agent with runtime = "agy" → rendered AGENTS.md
        // carries the AGY run_command protocol, not Claude's Monitor.
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        fs::write(
            agents_dir.join("design.md"),
            "# design\n\nrole.\n\n## Session Start\n\n1. Arm the Monitor via Monitor TOOL.\n\n## Convention\n\nx\n",
        )
        .unwrap();
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
runtime = "agy"
claudemd_template = "agents/design.md"
"#;
        let cfg = Config::load_str_for_test(cfg_text).unwrap();
        let body = render_agent_claudemd(
            &cfg,
            &cfg.agents[0],
            tmp.path(),
            &tmp.path().join("giga-harness.toml"),
        )
        .unwrap();
        assert!(
            body.contains("Runtime: Antigravity") && body.contains("run_command"),
            "agy snippet not injected:\n{body}",
        );
        assert!(
            !body.contains("Arm the Monitor via Monitor TOOL."),
            "stale Claude Monitor section survived:\n{body}",
        );
        // {{AGENT}} placeholder in the snippet got substituted
        assert!(body.contains("giga watch --as design --agy"));
        assert!(!body.contains("{{AGENT}}"));
        // role prose preserved
        assert!(body.contains("role."));
    }

    #[test]
    fn custom_template_claude_agent_keeps_monitor() {
        // Default runtime (no field) → Claude Monitor snippet injected.
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        fs::write(
            agents_dir.join("design.md"),
            "# design\n\nrole.\n\n{{SESSION_START}}\n",
        )
        .unwrap();
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
        let body = render_agent_claudemd(
            &cfg,
            &cfg.agents[0],
            tmp.path(),
            &tmp.path().join("giga-harness.toml"),
        )
        .unwrap();
        assert!(
            body.contains("Monitor` TOOL")
                || body.contains("Monitor TOOL")
                || body.contains("`Monitor`")
        );
        assert!(body.contains("giga watch --as design"));
        assert!(!body.contains("--agy") && !body.contains("--codex"));
    }
}
