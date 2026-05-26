//! `giga setup` — one-command bootstrap.
//!
//! Shells out to `claude` with a comprehensive baked-in prompt that
//! walks the user through scaffolding a multi-agent swarm. Eliminates
//! the README-paste step: every giga release ships with a prompt that
//! knows about *that release's* command surface and conventions.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use which::which;

pub fn run() -> Result<()> {
    if which("claude").is_err() {
        anyhow::bail!(
            "`claude` not found on PATH. Install Claude Code first:\n  \
             https://docs.claude.com/en/docs/claude-code/quickstart"
        );
    }

    let cwd = std::env::current_dir().context("getting current working directory")?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"));
    let configs_default = home.join(".giga").join("configs");
    let prompt = build_prompt(&cwd, &configs_default, current_platform_hint());

    let status = Command::new("claude")
        .arg(prompt)
        .status()
        .context("invoking claude")?;
    if !status.success() {
        anyhow::bail!("claude exited with {status}");
    }
    Ok(())
}

/// One-line description of the host OS for the bootstrap prompt.
/// Tells the bootstrap agent which `--terminal` mode to recommend.
fn current_platform_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS — use `giga launch --terminal mac-terminal` to open one Terminal.app window per agent"
    } else if cfg!(target_os = "linux") {
        "Linux — `giga launch` will use tmux by default (one session, N windows)"
    } else if cfg!(target_os = "windows") {
        "Windows — `giga launch` will use Windows Terminal (wt.exe) by default"
    } else {
        "unknown OS — `giga launch --terminal print` will print commands you can paste manually"
    }
}

/// Build the bootstrap prompt baked into `giga setup`. Pulled out of
/// `run()` so unit tests can verify all the format placeholders were
/// interpolated (a missing argument produces a literal `{cwd}` in the
/// output, which would silently break the bootstrap flow).
fn build_prompt(cwd: &Path, configs_default: &Path, platform_hint: &str) -> String {
    format!(
        "You are a giga-harness bootstrap agent running in a fresh Claude Code \
         session. The user just typed `giga setup` from `{cwd}`. giga v{ver} is \
         already installed and on PATH. Your job is to walk them through scaffolding \
         a multi-agent swarm — they should not need to read any external docs.\n\
         \n\
         ## What giga-harness is\n\
         \n\
         giga coordinates N parallel Claude Code sessions via append-only Markdown \
         files. One terminal per agent; each runs `claude` in its own workdir and \
         posts to shared inbox files. A `giga watch --as <slug>` monitor per agent \
         tails the channels they participate in and turns new messages into \
         notifications. No MCP server, no message bus — just files.\n\
         \n\
         ## Platform\n\
         \n\
         Detected: {platform_hint}.\n\
         \n\
         ## Step 1 — confirm prerequisites\n\
         \n\
         Run `giga --version`. If it errors, tell the user to (re)install giga via \
         the README one-liner: \
         https://github.com/mickfixesjunk/giga-harness#install\n\
         \n\
         ## Step 2 — ask the user 5 questions\n\
         \n\
         Use AskUserQuestion (one tool call, all five at once):\n\
         \n\
         1. **Project name** (kebab-case slug, e.g. `my-saas-side-project`). \
         Becomes the config dir name and the tmux session label.\n\
         2. **Which 2–4 agents** they want. Typical mixes: design+code+test, \
         design+code+test+review, code+test+review. Each agent is a slug + a \
         one-line role description. Suggest the typical mixes as options.\n\
         3. **Where their project code lives** (absolute path). Default to `{cwd}` \
         (their cwd). This becomes `code_root` for every agent.\n\
         4. **Topology**: single coordinator (recommended — typically `design` is \
         the hub, code and test talk only to design) vs. fully peer-to-peer \
         (every agent has a bilateral channel with every other agent).\n\
         5. **How to launch the agents**: pick the launcher mode for \
         `giga launch --terminal <MODE>`. Options:\n\
         * `mac-terminal` — one native Terminal.app window per agent (macOS only; \
         on this platform, default to this if detected).\n\
         * `tmux` — one tmux session, one window per agent (works on macOS and \
         Linux; lightweight, but agents share one window).\n\
         * `wt` — Windows Terminal, one tab per agent (Windows only).\n\
         * `auto` — let giga pick (wt → tmux → print).\n\
         Default the recommended option to whichever matches the platform \
         detected above.\n\
         \n\
         ## Step 3 — scaffold\n\
         \n\
         Create `{configs}/PROJECT_NAME/` with subdirs `agents/`, `inbox/`, and \
         `workdirs/<agent>/` for each agent. Write `giga-harness.toml` with:\n\
         \n\
         ```toml\n\
         [project]\n\
         name = \"PROJECT_NAME\"\n\
         \n\
         [paths]\n\
         wsl_inbox = \"{configs}/PROJECT_NAME/inbox\"\n\
         \n\
         [[agents]]\n\
         name = \"design\"\n\
         workdir = \"{configs}/PROJECT_NAME/workdirs/design\"\n\
         code_root = \"USER_CODE_ROOT\"\n\
         role = \"...\"\n\
         platform = \"wsl\"   # use \"wsl\" on macOS/Linux too — it just means \"unix paths\"\n\
         claudemd_template = \"agents/design.md\"\n\
         # ...repeat per agent...\n\
         \n\
         [[channels]]\n\
         file = \"code-design.md\"\n\
         side = \"wsl\"\n\
         participants = [\"code\", \"design\"]\n\
         purpose = \"Spec questions, scope refinements, implementation tradeoffs.\"\n\
         # ...one bilateral channel per peering, plus one broadcast...\n\
         \n\
         [[channels]]\n\
         file = \"_broadcast.md\"\n\
         side = \"wsl\"\n\
         participants = [\"design\", \"code\", \"test\"]   # all agents\n\
         purpose = \"Ecosystem-wide announcements.\"\n\
         ```\n\
         \n\
         Key invariants:\n\
         * `workdir` and `code_root` are DIFFERENT. Workdir is the agent's launch \
         context (CLAUDE.md lives there); code_root is where they actually edit \
         code. All agents share the same code_root.\n\
         * Channel filenames for bilateral channels: alphabetical, e.g. \
         `code-design.md` (not `design-code.md`).\n\
         * Broadcast channels start with `_` and include every agent.\n\
         * For coordinator topology, only create channels between the coordinator \
         and each other agent — NOT between peripheral agents.\n\
         \n\
         Then write one `agents/<slug>.md` per agent — their CLAUDE.md template. \
         Include: role, channel table, Session Start instructions (run \
         `giga watch --as <slug> --config <full-toml-path>` to tail channels), and \
         the message format convention (every message ends with `WAITING ON: <agent>` \
         or `(Informational, no response required.)`).\n\
         \n\
         ## Step 4 — discover the command surface\n\
         \n\
         Run these to confirm the commands exist in this giga version:\n\
         * `giga --help`\n\
         * `giga add-agent --help` (if the user later wants to add agents)\n\
         * `giga validate --help`\n\
         * `giga init --help`\n\
         * `giga launch --help`\n\
         \n\
         ## Step 5 — validate, init, launch\n\
         \n\
         From the config dir:\n\
         ```\n\
         cd {configs}/PROJECT_NAME\n\
         giga validate            # confirms TOML is well-formed\n\
         giga init                # creates inbox files + per-workdir CLAUDE.md, \
         pre-trusts the dirs in ~/.claude.json\n\
         giga launch --terminal CHOSEN_MODE  # opens one terminal per agent (use the mode from question 5)\n\
         ```\n\
         \n\
         ## Step 6 — confirm with the user\n\
         \n\
         After `giga launch` succeeds, tell the user:\n\
         * Where their config lives (`{configs}/PROJECT_NAME/giga-harness.toml`)\n\
         * Where each agent is running\n\
         * That each agent will auto-arm its `giga watch` monitor and post a \
         hello on its channels as part of its Session Start protocol — no manual \
         setup needed per terminal.\n\
         * Just switch to the coordinator's terminal (design) and give it the \
         first task to scope. It'll route to the other agents from there.\n\
         \n\
         ## Fallback / reference\n\
         \n\
         If anything is ambiguous: \
         https://github.com/mickfixesjunk/giga-harness/blob/main/MANUAL_SETUP.md \
         is the full conventions doc.\n\
         \n\
         Begin now: confirm prerequisites, then ask the user the four questions.",
        cwd = cwd.display(),
        ver = env!("CARGO_PKG_VERSION"),
        platform_hint = platform_hint,
        configs = configs_default.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_prompt() -> String {
        build_prompt(
            Path::new("/Users/me/code/myproj"),
            Path::new("/Users/me/.giga/configs"),
            "macOS — sample hint",
        )
    }

    #[test]
    fn prompt_contains_no_unresolved_placeholders() {
        // The format!() call has named args (`cwd`, `ver`, `platform_hint`,
        // `configs`). If anyone removes one of the bindings or adds a
        // new `{...}` without binding it, format! errors at compile-time
        // — but a typo like `{cwd2}` could slip through as a literal.
        // Guard against that.
        let out = sample_prompt();
        assert!(!out.contains("{cwd}"), "unresolved {{cwd}} in prompt");
        assert!(!out.contains("{ver}"), "unresolved {{ver}} in prompt");
        assert!(
            !out.contains("{platform_hint}"),
            "unresolved {{platform_hint}} in prompt"
        );
        assert!(
            !out.contains("{configs}"),
            "unresolved {{configs}} in prompt"
        );
    }

    #[test]
    fn prompt_interpolates_cwd() {
        let out = sample_prompt();
        assert!(
            out.contains("/Users/me/code/myproj"),
            "cwd not in prompt — bootstrap agent won't know where the user is"
        );
    }

    #[test]
    fn prompt_interpolates_configs_default() {
        let out = sample_prompt();
        assert!(
            out.contains("/Users/me/.giga/configs"),
            "configs default path not in prompt — bootstrap agent might pick the wrong location"
        );
    }

    #[test]
    fn prompt_interpolates_platform_hint() {
        let out = sample_prompt();
        assert!(out.contains("macOS — sample hint"));
    }

    #[test]
    fn prompt_includes_giga_version() {
        // Pinning the version into the prompt makes the bootstrap
        // agent aware of what command surface to expect. If the
        // env! lookup ever breaks, the prompt would have a literal
        // empty string here.
        let out = sample_prompt();
        assert!(
            out.contains(env!("CARGO_PKG_VERSION")),
            "compiled-in giga version is missing from prompt"
        );
    }

    #[test]
    fn prompt_references_all_five_questions() {
        let out = sample_prompt();
        // The bootstrap flow hinges on these five questions being
        // mentioned. If a future edit accidentally drops one, this
        // test catches it.
        assert!(out.contains("Project name"));
        assert!(out.contains("Which 2"));
        assert!(out.contains("project code lives"));
        assert!(out.contains("Topology"));
        assert!(out.contains("launch the agents"));
    }

    #[test]
    fn prompt_mentions_code_root_separation() {
        // Bootstrap must scaffold with code_root (workdir != codebase).
        // If this guidance gets dropped the agent will fall back to
        // the old pattern of dumping CLAUDE.md into the codebase.
        let out = sample_prompt();
        assert!(out.contains("code_root"));
        assert!(out.contains("workdir"));
    }

    #[test]
    fn platform_hint_picks_correct_string_for_host() {
        // Compile-time selection — just verify it returns something
        // sensible for whichever OS the tests are running on.
        let hint = current_platform_hint();
        assert!(!hint.is_empty());
        if cfg!(target_os = "macos") {
            assert!(hint.contains("mac-terminal"));
        } else if cfg!(target_os = "linux") {
            assert!(hint.contains("tmux"));
        } else if cfg!(target_os = "windows") {
            assert!(hint.contains("Windows Terminal"));
        }
    }
}
