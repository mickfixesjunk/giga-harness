//! `giga launch` — spawn one terminal per agent and start each one
//! in their working directory with `claude` (if installed) ready to go.

use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::init;
use crate::terminal::{self, Multiplexer, Pane};

pub fn run(
    config_path: &Path,
    skip_init: bool,
    dry_run: bool,
    only: &[String],
    new_window: bool,
    terminal: &str,
) -> Result<()> {
    if !skip_init {
        init::run(config_path)?;
        println!();
    }

    let cfg = Config::load(config_path)?;
    let session = format!("giga-{}", cfg.project.name);

    // The intro prompt is what each CLI session processes the moment
    // it opens. Generic by design — per-agent behavior lives in each
    // agent's AGENTS.md (which the prompt references). A project-level
    // `launch_intro_prompt` overrides for ALL agents; otherwise we
    // pick a runtime-appropriate default per agent below.
    let intro_override = cfg.project.launch_intro_prompt.as_deref();

    // If --only was passed, narrow the agent list to that set and
    // error on any name the config doesn't know — typos here are
    // common and silent skips would be worse than a hard failure.
    let name_filtered: Box<dyn Iterator<Item = &_>> = if only.is_empty() {
        Box::new(cfg.agents.iter())
    } else {
        let known: Vec<&str> = cfg.agents.iter().map(|a| a.name.as_str()).collect();
        let unknown: Vec<&str> = only
            .iter()
            .map(String::as_str)
            .filter(|n| !known.contains(n))
            .collect();
        if !unknown.is_empty() {
            anyhow::bail!(
                "--only names unknown agent(s): {} — known agents: {}",
                unknown.join(", "),
                known.join(", "),
            );
        }
        Box::new(
            cfg.agents
                .iter()
                .filter(|a| only.iter().any(|n| n == &a.name)),
        )
    };

    // v0.6.6: host-aware filter. Skip agents whose `host` doesn't
    // match this_host so launch on a multi-host swarm only spawns
    // panes for agents that actually live here. Pre-fix: Mick saw
    // `giga launch` on TRINITY spawn WT panes for all 4 morpheus
    // agents alongside the 4 trinity agents — they failed because
    // the workdirs only exist on the peer. Same class as init's
    // v0.3.4 F9 fix (which filtered scaffolding but launch didn't).
    //
    // For legacy local-only swarms (no this_host) → no filter.
    // Collect into Vec rather than chaining iterators so cfg can be
    // moved/borrowed later in run() without lifetime gymnastics.
    let local_agents: Vec<&crate::config::Agent> = match cfg.this_host.as_deref() {
        Some(th) => name_filtered
            .filter(|a| cfg.agent_host(a).map(|h| h == th).unwrap_or(false))
            .collect(),
        None => name_filtered.collect(),
    };
    let skipped_count = if let Some(th) = cfg.this_host.as_deref() {
        cfg.agents
            .iter()
            .filter(|a| {
                (only.is_empty() || only.iter().any(|n| n == &a.name))
                    && cfg.agent_host(a).map(|h| h != th).unwrap_or(false)
            })
            .count()
    } else {
        0
    };
    if skipped_count > 0 {
        println!(
            "  (skipping {skipped_count} peer-host agent(s) — they live on other hosts)"
        );
    }
    let agents_iter: Box<dyn Iterator<Item = &_>> = Box::new(local_agents.into_iter());

    let mut panes: Vec<Pane> = agents_iter
        .flat_map(|a| {
            let runtime = cfg.agent_runtime(a);
            let cwd = a.workdir.to_string_lossy().to_string();
            let base_intro = intro_override.unwrap_or_else(|| runtime.launch_intro_prompt());
            let agent_intro = intro_for_agent(base_intro, a);
            // Per-agent launch_cmd override wins; otherwise pick a
            // runtime-appropriate default.
            let cmd = a.launch_cmd.clone().unwrap_or_else(|| {
                default_cmd_for_runtime(
                    runtime,
                    &a.platform,
                    &agent_intro,
                    &cfg.project.launch_model,
                )
            });
            // v0.6.0: codex-runtime agents get TWO panes: the CLI pane
            // (named `<agent>-cli`) and a bridge sidecar (named
            // `<agent>-bridge`) running `giga watch --codex` with
            // CODEX_CHANNEL_DIR pointing at the per-agent inbox tree.
            // Other runtimes get the single-pane shape titled `<agent>`.
            let mut out: Vec<Pane> = Vec::new();
            if runtime.needs_bridge_pane() {
                let bridge_dir = a.workdir.join("codex-channel");
                let bridge_dir_unix = bridge_dir.display().to_string();
                let bridge_cmd = format!(
                    "CODEX_CHANNEL_DIR={} giga watch --as {} --codex",
                    shell_escape::unix::escape(std::borrow::Cow::Borrowed(bridge_dir_unix.as_str())),
                    a.name,
                );
                out.push(Pane {
                    title: format!("{}-bridge", a.name),
                    cwd: cwd.clone(),
                    cmd: bridge_cmd,
                    platform: a.platform.clone(),
                    admin: a.admin,
                });
                out.push(Pane {
                    title: format!("{}-cli", a.name),
                    cwd,
                    cmd,
                    platform: a.platform.clone(),
                    admin: a.admin,
                });
            } else {
                out.push(Pane {
                    title: a.name.clone(),
                    cwd,
                    cmd,
                    platform: a.platform.clone(),
                    admin: a.admin,
                });
            }
            out
        })
        .collect();

    let incremental = !only.is_empty();

    // Cross-host swarms need two extra long-running daemons per host:
    //   - giga sync (rsync slices + canonical TOML to peers)
    //   - giga merger (append peer slices into local merged file)
    // We add them as additional panes alongside the agent panes — visible
    // in the multiplexer, so the user can see their logs and notice if
    // they die.
    //
    // v0.3.4 (quality F11): spawn the daemons even on --only launches.
    // Previously --only set incremental=true and skipped them on the
    // theory "the original full launch already started them". But the
    // common --only path is `giga launch --only <new-agent>` to add a
    // single agent to an existing session — and if this is the FIRST
    // agent on this host (no prior full launch happened here), the
    // daemons never start and the named agent is isolated. Quality's
    // repro: morpheus-wsl had no prior `giga launch` (init was broken
    // by a different finding), they ran `--only performance`, and
    // sync/merger were silently missing.
    //
    // Trade-off: re-running --only when daemons ARE already alive
    // produces a duplicate giga-sync + giga-merger pane. Both are
    // idempotent (sync is rsync no-ops; merger is append-with-mtime),
    // so the cost is "extra pane to clean up" rather than data damage.
    // Acceptable until we add per-host daemon presence detection.
    if should_spawn_daemons_v2(&cfg, only) {
        // v0.6.4-class fix: derive swarm_dir from CANONICALIZED config
        // path so the daemon panes get the actual swarm dir as cwd
        // even when launch was invoked via a workdir-side symlink.
        // Pre-fix Mick saw "bash: cd: null directory" because the
        // symlink resolved to a workdir parent that produced empty/null.
        let canonical = config_path
            .canonicalize()
            .unwrap_or_else(|_| config_path.to_path_buf());
        let swarm_dir = canonical
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        panes.push(daemon_pane("giga-sync", "giga sync", &swarm_dir));
        panes.push(daemon_pane("giga-merger", "giga merger", &swarm_dir));
    }
    let _ = incremental;
    let mux = terminal::parse_override(terminal).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown --terminal value `{}` — valid: auto, tmux, mac-terminal, wt, print",
            terminal
        )
    })?;
    let mut tags = Vec::new();
    if incremental {
        tags.push("incremental");
    }
    if new_window {
        tags.push("new-window");
    }
    let tag_str = if tags.is_empty() {
        String::new()
    } else {
        format!(" ({})", tags.join(", "))
    };
    println!("multiplexer: {mux:?}");
    println!("session:     {session}{tag_str}");
    println!("panes:       {}", panes.len());
    for p in &panes {
        println!("  - {} ({}) — cwd={}", p.title, p.platform, p.cwd);
    }

    if dry_run {
        println!("\n(dry-run — not spawning)");
        return Ok(());
    }

    if mux == Multiplexer::None {
        eprintln!("\nwarning: no multiplexer available — printing commands instead");
    }

    terminal::launch(mux, &panes, &session, incremental, new_window)?;
    Ok(())
}

// The per-runtime default opening prompt lives on the `Runtime` enum
// (see `Runtime::launch_intro_prompt`) and is sourced from
// `templates/runtimes/<runtime>-intro.md` at compile time. Project
// configs can override via `[project].launch_intro_prompt`.
//
// IMPORTANT: those intro files must NOT contain backticks. Backticks
// survive single-quoting on the wt.exe → wsl.exe → bash hop and end
// up shell-evaluated as command substitution, corrupting the prompt
// the agent actually receives. Tests in `runtime.rs` enforce this.

/// True when the launcher should add `giga-sync` + `giga-merger` panes
/// for this run. Multi-host swarms need them; local-only swarms don't.
///
/// v0.3.4 (quality F11): always spawn on multi-host even when
/// `--only` is set. The previous "incremental skips daemons" logic
/// assumed a prior full launch had already started them, but the
/// common --only flow is also the FIRST launch on a freshly bootstrapped
/// peer, where no daemons exist yet. False-skipping silently isolated
/// the agent. `incremental` is kept as a parameter so future tuning
/// (e.g., adding presence-detection to skip duplicates) has the signal.
///
/// v0.3.6 (SWARM_BOSS_DESIGN.md): when an agent on THIS host is flagged
/// `swarm_boss = true`, that agent's session will arm sync + merger
/// Monitors at startup — so suppress the tmux daemon panes here to
/// avoid duplicate daemons. Per-host scoped: a peer host's swarm_boss
/// doesn't affect this host's launch decision.
fn should_spawn_daemons(cfg: &crate::config::Config, _incremental: bool) -> bool {
    should_spawn_daemons_v2(cfg, &[])
}

/// v0.6.5: refined daemon-spawn rule (per Mick 2026-06-02). Daemons
/// are needed only when there's actual cross-host work to coordinate.
/// Per-rule decision:
///
/// | Scenario                                    | Spawn? |
/// |---------------------------------------------|--------|
/// | Local-only swarm (no [[hosts]])             | NO     |
/// | Has [[hosts]] but no peers (single host)    | NO     |
/// | Peers + swarm_boss on this_host             | NO (boss handles via Monitor) |
/// | Peers + no boss + full launch (no --only)   | YES (last-resort bootstrap)   |
/// | Peers + no boss + --only set                | NO  (operator knows; daemons should already be running) |
///
/// Last row is Mick's complaint: `giga launch --only codex-review`
/// on a swarm with no boss was spawning daemons unnecessarily —
/// the operator wasn't bootstrapping, just adding an agent. The
/// daemons either already exist (run them manually OR via boss) or
/// the operator does `giga launch` (no --only) for a fresh bootstrap.
fn should_spawn_daemons_v2(cfg: &crate::config::Config, only: &[String]) -> bool {
    if cfg.hosts.is_empty() {
        return false;
    }
    // No peers (single-host swarm with [[hosts]] populated) → daemons
    // have nothing to do.
    let has_peers = match cfg.this_host.as_deref() {
        Some(this) => cfg.hosts.iter().any(|h| h.name != this),
        None => !cfg.hosts.is_empty(),
    };
    if !has_peers {
        return false;
    }
    // Boss on this_host owns the daemons via Monitor entries in its
    // AGENTS.md → no tmux daemon panes from launch.
    if let Some(this) = cfg.this_host.as_deref() {
        let has_local_boss = cfg.agents.iter().any(|a| {
            a.swarm_boss && cfg.agent_host(a).map(|h| h == this).unwrap_or(false)
        });
        if has_local_boss {
            return false;
        }
    }
    // No boss configured. Daemons need tmux panes — but only if this
    // is a full launch (operator bootstrapping). --only launches are
    // operator-knows-what-they're-doing and should NOT spawn daemons
    // (avoids duplicates, matches "daemons are explicitly the boss's
    // job" mental model).
    only.is_empty()
}

/// Build a multiplexer pane for one of the per-host background daemons
/// (sync / merger). Always WSL-platform in v1 (Mick's hosts are all
/// WSL/Linux); cwd is the swarm config dir so the daemon picks up the
/// right giga-harness.toml via the default resolution. No claude
/// involvement — these tabs just run the daemon and show its logs.
fn daemon_pane(title: &str, cmd: &str, swarm_dir: &str) -> Pane {
    Pane {
        title: title.to_string(),
        cwd: swarm_dir.to_string(),
        cmd: cmd.to_string(),
        platform: "wsl".to_string(),
        admin: false,
    }
}

/// v0.6.0: per-runtime default command. Dispatches on the agent's
/// runtime; Claude keeps the existing two-attempt resume-or-fresh
/// shape; Codex and Agy use their simpler `echo intro | cli`
/// pattern (these CLIs read stdin for the initial prompt and don't
/// have a Claude-style session-resume flag).
fn default_cmd_for_runtime(
    runtime: crate::runtime::Runtime,
    platform: &str,
    intro: &str,
    model: &str,
) -> String {
    match runtime {
        crate::runtime::Runtime::Claude => default_cmd_claude(platform, intro, model),
        // v0.6.5: codex stays plain — intro delivered via the
        // codex-channel bridge envelope mechanism, not via CLI.
        crate::runtime::Runtime::Codex => default_cmd_tty_only("codex", platform),
        // v0.6.8: agy has native -i / --prompt-interactive for initial
        // prompt + interactive session. Use it so the agent boots
        // with the intro that tells it to read AGENTS.md, follow
        // Session Start protocol, etc. Pre-v0.6.8 agy launched plain
        // and the agent never saw the intro → boots generic.
        crate::runtime::Runtime::Agy => default_cmd_agy_interactive(platform, intro),
    }
}

/// v0.6.5: launch a TUI-style CLI that REQUIRES an interactive TTY
/// with NO initial prompt. Used by codex (intro arrives via the
/// codex-channel bridge envelope, not the CLI). Wraps with
/// `command -v` so missing binaries fail visibly rather than
/// leaving an empty interactive shell.
fn default_cmd_tty_only(bin: &str, platform: &str) -> String {
    match platform {
        "windows" => {
            format!(
                "if (Get-Command {bin} -ErrorAction SilentlyContinue) {{ {bin} }}",
            )
        }
        _ => {
            format!("command -v {bin} >/dev/null && {bin} || true")
        }
    }
}

/// v0.6.8: agy supports `-i / --prompt-interactive <prompt>` for
/// "run an initial prompt interactively and continue the session".
/// This is the equivalent of `claude -c <intro>` — gives agy the
/// intro that tells the agent to read AGENTS.md, arm the watcher,
/// etc. Without this the agent boots with no context.
fn default_cmd_agy_interactive(platform: &str, intro: &str) -> String {
    match platform {
        "windows" => {
            let ps_intro = intro.replace('\'', "''");
            format!(
                "if (Get-Command agy -ErrorAction SilentlyContinue) {{ \
                   agy -i '{ps_intro}' \
                 }}",
            )
        }
        _ => {
            let sh_intro = shell_escape::unix::escape(intro.into());
            format!("command -v agy >/dev/null && agy -i {sh_intro} || true")
        }
    }
}

/// Platform-appropriate default shell command for Claude agents.
/// Tries `claude -c` first to resume the most-recent session in this
/// cwd; falls back to `claude` (fresh session) if `-c` fails — which
/// it does on the first launch of a brand-new agent, where no prior
/// session exists. (Claude Code's `-c` errors with "No conversation
/// found to continue" rather than starting fresh, so we have to
/// handle that here.)
fn default_cmd_claude(platform: &str, intro: &str, model: &str) -> String {
    match platform {
        "windows" => {
            // PowerShell. Single-quote the intro and double any inner
            // single quotes (PS's `''` escape). Wrap the resume + new
            // attempts so a `-c` failure falls through to a fresh
            // session with the same intro.
            let ps_intro = intro.replace('\'', "''");
            let ps_model = model.replace('\'', "''");
            format!(
                "if (Get-Command claude -ErrorAction SilentlyContinue) {{ \
                   claude -c --model '{ps_model}' '{ps_intro}'; \
                   if ($LASTEXITCODE -ne 0) {{ claude --model '{ps_model}' '{ps_intro}' }} \
                 }}",
            )
        }
        _ => {
            // POSIX bash. shell_escape gives us a safely-quoted form.
            // Group the resume + new attempts with `{ ... ; }` so the
            // outer `|| true` only fires if claude is missing entirely.
            let sh_intro = shell_escape::unix::escape(intro.into());
            let sh_model = shell_escape::unix::escape(model.into());
            format!(
                "command -v claude >/dev/null && \
                 {{ claude -c --model {sh_model} {sh_intro} || claude --model {sh_model} {sh_intro} ; }} || true",
            )
        }
    }
}

/// Build the intro prompt for one agent. Composes:
///   1. An identity preamble — tells the agent its slug and the
///      hard rule that every reply must start with `[<slug>]`.
///   2. The project-level intro (HANDOVER.md handling, session-start
///      protocol pointer, etc.).
///   3. A code-root note if the agent has one set.
///
/// Extracted from `run()` so the wiring is testable without spawning
/// terminals. The identity rule is reinforced in AGENTS.md as well so
/// it survives session restarts — but this is what the agent sees on
/// the very first turn, before it's read its AGENTS.md.
pub(crate) fn intro_for_agent(intro: &str, agent: &crate::config::Agent) -> String {
    // See `Runtime::launch_intro_prompt` — no backticks in any string
    // that ends up on a shell command line. They get shell-evaluated
    // on the wt → wsl → bash hop.
    let identity = format!(
        "You are the {slug} agent in this giga-harness swarm. EVERY response \
         you make to the user in this terminal MUST start with [{slug}] so the \
         user knows which agent is talking — this applies to every assistant turn, \
         not just channel messages. ",
        slug = agent.name,
    );
    if let Some(cr) = &agent.code_root {
        format!(
            "{identity}{intro} Your code root (where all code work happens) is {cr} — when you start editing code (LATER, not during session-start), cd there first. Until then stay in your launch cwd; AGENTS.md and HANDOVER.md live in cwd, NOT in the code root.",
            identity = identity,
            intro = intro,
            cr = cr.display(),
        )
    } else {
        format!("{identity}{intro}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Agent;
    use std::path::PathBuf;

    fn agent_named(name: &str, code_root: Option<&str>) -> Agent {
        Agent {
            name: name.to_string(),
            workdir: PathBuf::from(format!("/h/{name}")),
            role: "test".into(),
            platform: "wsl".into(),
            host: None,
            bench_scheduler: false,
            claudemd_template: None,
            launch_cmd: None,
            code_root: code_root.map(PathBuf::from),
            admin: false,
            swarm_boss: false,
            runtime: None,
        }
    }

    #[test]
    fn intro_identifies_the_agent_by_slug() {
        let a = agent_named("design", None);
        let out = intro_for_agent("base intro.", &a);
        assert!(out.contains("design agent"), "missing identity:\n{out}");
    }

    #[test]
    fn intro_demands_bracketed_reply_prefix() {
        // The [slug] prefix rule is what lets the user tell which
        // window/agent is responding. Don't let a future refactor
        // silently drop it.
        let a = agent_named("code", None);
        let out = intro_for_agent("base.", &a);
        assert!(out.contains("[code]"), "reply-prefix rule missing:\n{out}");
        assert!(out.contains("EVERY response"), "rule wording softened?");
    }

    #[test]
    fn intro_never_contains_backticks() {
        // Backticks survive single-quoting through the wt.exe → wsl.exe
        // → bash hop and get evaluated as command substitution, which
        // corrupts the prompt Claude actually receives. Lock this out.
        let a = agent_named("code", Some("/code/myproj"));
        let out = intro_for_agent("base intro with no ticks.", &a);
        assert!(
            !out.contains('`'),
            "backtick leaked into intro — will be shell-evaluated:\n{out}",
        );
    }

    #[test]
    fn intro_preserves_base_intro_verbatim() {
        let a = agent_named("design", None);
        let base = "If HANDOVER.md exists, read it. Otherwise follow Session Start.";
        let out = intro_for_agent(base, &a);
        assert!(out.contains(base), "base intro got mangled");
    }

    #[test]
    fn intro_appends_code_root_clause_when_set() {
        let a = agent_named("code", Some("/code/myproj"));
        let out = intro_for_agent("base.", &a);
        assert!(out.contains("/code/myproj"));
        assert!(
            out.contains("cd there"),
            "code_root clause should tell the agent to cd:\n{out}",
        );
        // Regression guard: the cd must be deferred so agents don't
        // immediately cd out of their workdir on session start (which
        // hides AGENTS.md / HANDOVER.md and triggers filesystem-wide
        // hunting). Burned on agy/coder 2026-06-02.
        assert!(
            out.contains("LATER")
                || out.contains("later")
                || out.contains("when you start editing"),
            "code_root clause must defer the cd, not demand it up front:\n{out}",
        );
        assert!(
            out.contains("AGENTS.md") && out.contains("cwd"),
            "code_root clause must remind agent that AGENTS.md lives in cwd:\n{out}",
        );
    }

    #[test]
    fn intro_omits_code_root_clause_when_unset() {
        let a = agent_named("code", None);
        let out = intro_for_agent("base.", &a);
        assert!(
            !out.contains("code root"),
            "code_root language leaked into intro when field is None:\n{out}",
        );
    }

    #[test]
    fn intro_for_distinct_agents_uses_distinct_slugs() {
        // Regression guard: if the formatter ever closed over the wrong
        // variable, both agents could end up with the same slug.
        let a = intro_for_agent("base.", &agent_named("design", None));
        let b = intro_for_agent("base.", &agent_named("code", None));
        assert!(a.contains("design agent") && !a.contains("code agent"));
        assert!(b.contains("code agent") && !b.contains("design agent"));
    }

    /// v0.3.4 fix for quality finding 11: --only on a multi-host swarm
    /// must STILL spawn sync + merger daemons. Pre-fix: `incremental`
    /// (set by --only) suppressed them, leaving the named agent
    /// isolated when --only was the first launch on the host.
    #[test]
    fn daemons_spawn_on_multi_host_even_when_incremental() {
        // Need a temp dir + this_host.toml so multi-host validation passes.
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_text = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "host-a"
"#;
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg_path, cfg_text).unwrap();
        std::fs::write(tmp.path().join("this_host.toml"), "this_host = \"host-a\"\n").unwrap();
        let cfg = crate::config::Config::load(&cfg_path).unwrap();
        assert!(
            should_spawn_daemons(&cfg, true),
            "incremental + multi-host must still spawn daemons"
        );
        assert!(
            should_spawn_daemons(&cfg, false),
            "full launch on multi-host spawns daemons (baseline)"
        );
    }

    #[test]
    fn daemons_not_spawned_on_local_only_swarm() {
        // Pre-existing invariant: local-only swarm (no [[hosts]]) doesn't
        // need sync/merger. The F11 fix must not regress this.
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
"#;
        let cfg = crate::config::Config::load_str_for_test(body).unwrap();
        assert!(!should_spawn_daemons(&cfg, false));
        assert!(!should_spawn_daemons(&cfg, true));
    }

    /// v0.3.6 S5 (SWARM_BOSS_DESIGN.md): when an agent on this host
    /// is flagged swarm_boss, tmux daemons are suppressed (the boss
    /// agent will arm them as Monitors instead).
    #[test]
    fn daemons_suppressed_when_swarm_boss_present_on_this_host() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_text = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"

[[agents]]
name = "boss-a"
workdir = "/h/boss-a"
role = "."
platform = "wsl"
host = "host-a"
swarm_boss = true

[[agents]]
name = "agent-a"
workdir = "/h/agent-a"
role = "."
platform = "wsl"
host = "host-a"
"#;
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg_path, cfg_text).unwrap();
        std::fs::write(tmp.path().join("this_host.toml"), "this_host = \"host-a\"\n").unwrap();
        let cfg = crate::config::Config::load(&cfg_path).unwrap();
        assert!(
            !should_spawn_daemons(&cfg, false),
            "swarm_boss on this_host -> tmux daemons suppressed"
        );
        assert!(
            !should_spawn_daemons(&cfg, true),
            "swarm_boss suppression applies in --only mode too"
        );
    }

    /// v0.3.6 S6: swarm_boss on a PEER host doesn't affect this host's
    /// daemon-spawn decision. Each host scoped independently.
    #[test]
    fn daemons_still_spawn_when_swarm_boss_is_only_on_peer_host() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_text = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"

[[agents]]
name = "agent-a"
workdir = "/h/agent-a"
role = "."
platform = "wsl"
host = "host-a"

[[agents]]
name = "boss-b"
workdir = "/h/boss-b"
role = "."
platform = "wsl"
host = "host-b"
swarm_boss = true
"#;
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg_path, cfg_text).unwrap();
        std::fs::write(tmp.path().join("this_host.toml"), "this_host = \"host-a\"\n").unwrap();
        let cfg = crate::config::Config::load(&cfg_path).unwrap();
        assert!(
            should_spawn_daemons(&cfg, false),
            "boss only on peer host -> we still need our own tmux daemons"
        );
    }

    #[test]
    fn daemon_pane_targets_swarm_dir_and_runs_command_verbatim() {
        let p = daemon_pane("giga-sync", "giga sync", "/home/me/.giga/configs/test-swarm");
        assert_eq!(p.title, "giga-sync");
        assert_eq!(p.cwd, "/home/me/.giga/configs/test-swarm");
        assert_eq!(p.cmd, "giga sync");
        assert_eq!(p.platform, "wsl");
        assert!(!p.admin);
    }

    /// v0.6.5: should_spawn_daemons_v2 returns false when [[hosts]]
    /// is populated but has no peers (single-host swarm with
    /// [[hosts]] declared). Mick's superdeduper case.
    #[test]
    fn daemons_suppressed_when_no_peers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_text = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "host-a"
"#;
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg_path, cfg_text).unwrap();
        std::fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"host-a\"\n",
        )
        .unwrap();
        let cfg = crate::config::Config::load(&cfg_path).unwrap();
        assert!(!should_spawn_daemons_v2(&cfg, &[]));
        assert!(!should_spawn_daemons_v2(&cfg, &["alice".to_string()]));
    }

    /// v0.6.5: with peers + no boss + --only set, daemons skipped
    /// (operator's adding agents, not bootstrapping).
    #[test]
    fn daemons_skipped_in_only_mode_when_no_boss() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_text = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "host-a"
"#;
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg_path, cfg_text).unwrap();
        std::fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"host-a\"\n",
        )
        .unwrap();
        let cfg = crate::config::Config::load(&cfg_path).unwrap();
        // Full launch (no --only) → still spawn daemons.
        assert!(should_spawn_daemons_v2(&cfg, &[]));
        // --only set without a boss → skip daemons.
        assert!(!should_spawn_daemons_v2(&cfg, &["alice".to_string()]));
    }
}
