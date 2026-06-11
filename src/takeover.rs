//! `giga takeover` — flip an agent's runtime, regenerate its workdir
//! AGENTS.md, find the prior runtime's session log, and append a
//! takeover block to HANDOVER.md so the new agent reads everything
//! it needs on session start.
//!
//! Designed for the "one-shot prompt" UX: the operator starts a fresh
//! CLI in the existing agent's workdir and says "use giga to take
//! over from this <old-runtime> agent". The new agent runs
//! `giga takeover` (no flags — slug auto-detected from cwd, target
//! runtime defaults to `claude`), then follows the regenerated
//! AGENTS.md + the takeover block now at the top of HANDOVER.md.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use toml_edit::{value, DocumentMut};

use crate::config::{Agent, Config};
use crate::runtime::Runtime;

pub struct Args {
    /// Path to the swarm's giga-harness.toml. Defaults via
    /// `registry::resolve_config` like every other subcommand.
    pub config: PathBuf,
    /// Override the agent slug. If None, takeover auto-detects the
    /// agent by matching cwd to one of the agent workdirs in config.
    pub as_agent: Option<String>,
    /// Target runtime for the new agent (default: claude — most
    /// common takeover tool is Claude Code).
    pub to_runtime: Runtime,
    /// Print the plan; don't touch any file.
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let cfg = Config::load(&args.config)?;
    let abs_config = fs::canonicalize(&args.config).unwrap_or(args.config.clone());

    // 1. Resolve the target agent.
    let slug = match args.as_agent.clone() {
        Some(s) => s,
        None => detect_slug_from_cwd(&cfg)?,
    };
    let agent = cfg
        .agents
        .iter()
        .find(|a| a.name == slug)
        .ok_or_else(|| anyhow!("agent `{slug}` not found in {}", args.config.display()))?
        .clone();

    let old_runtime = cfg.agent_runtime(&agent);
    let new_runtime = args.to_runtime;

    if old_runtime == new_runtime {
        println!(
            "agent `{slug}` is already on runtime `{}`; nothing to do",
            new_runtime.as_str(),
        );
        return Ok(());
    }

    println!(
        "==> takeover: `{slug}` (workdir `{}`)",
        agent.workdir.display()
    );
    println!(
        "    from runtime `{}` -> `{}`",
        old_runtime.as_str(),
        new_runtime.as_str(),
    );

    // 2. Locate the prior runtime's session log for this workdir. The
    //    new agent will read it to absorb what the old one was doing.
    let session_hint = locate_session_file(old_runtime, &agent.workdir);
    match &session_hint {
        Some(p) => println!("    prior session log: {}", p.display()),
        None => println!(
            "    prior session log: (not found — the `{}` CLI may not \
             keep a recoverable per-workdir log on this host)",
            old_runtime.as_str(),
        ),
    }

    if args.dry_run {
        println!("\n(dry-run — not touching TOML, AGENTS.md, or HANDOVER.md)");
        return Ok(());
    }

    // 3. Flip the runtime in TOML. toml_edit preserves comments +
    //    surrounding formatting.
    update_agent_runtime_in_toml(&abs_config, &slug, new_runtime)?;
    println!("  + TOML updated (runtime field on `{slug}`)");

    // 4. Re-render the workdir AGENTS.md for the new runtime. We
    //    re-load Config so the runtime change is reflected.
    let cfg_after = Config::load(&abs_config)?;
    let agent_after = cfg_after
        .agents
        .iter()
        .find(|a| a.name == slug)
        .ok_or_else(|| anyhow!("agent `{slug}` vanished after TOML write"))?;
    let config_dir = abs_config
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let body =
        crate::init::render_agent_claudemd(&cfg_after, agent_after, &config_dir, &abs_config)?;
    let agents_md = agent_after.workdir.join("AGENTS.md");
    fs::write(&agents_md, body).with_context(|| format!("writing {}", agents_md.display()))?;
    println!("  + AGENTS.md re-rendered for `{}`", new_runtime.as_str());

    // 5. Prepend the takeover block to HANDOVER.md so it lands at the
    //    top of the new agent's read on session start.
    let handover_path = agent_after.workdir.join("HANDOVER.md");
    let block = render_takeover_block(&slug, old_runtime, new_runtime, session_hint.as_deref());
    prepend_to_file(&handover_path, &block)
        .with_context(|| format!("updating {}", handover_path.display()))?;
    println!("  + HANDOVER.md updated (takeover block at top)");

    // 6. Print the one-shot takeover prompt the new agent should
    //    follow. Self-contained — the new agent can copy this into
    //    its own turn-1 plan with no other context.
    println!();
    println!("== takeover prompt (the new agent should follow this) ==");
    println!(
        "{}",
        takeover_prompt(&slug, old_runtime, new_runtime, session_hint.as_deref())
    );

    Ok(())
}

/// Auto-detect the agent slug for the takeover by matching cwd to an
/// agent's workdir. Errors clearly if cwd doesn't match any agent or
/// matches more than one (which would be a config bug — workdirs are
/// supposed to be unique per agent).
fn detect_slug_from_cwd(cfg: &Config) -> Result<String> {
    let cwd = std::env::current_dir().context("getting cwd")?;
    let cwd_canon = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
    let mut matches: Vec<&Agent> = cfg
        .agents
        .iter()
        .filter(|a| {
            let wd = a
                .workdir
                .canonicalize()
                .unwrap_or_else(|_| a.workdir.clone());
            wd == cwd_canon
        })
        .collect();
    match matches.len() {
        0 => Err(anyhow!(
            "cwd ({}) doesn't match any agent's workdir in {}. \
             Run takeover from the agent's workdir, or pass --as <slug>.",
            cwd.display(),
            cfg.source_path
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<unknown config>".to_string()),
        )),
        1 => Ok(matches.remove(0).name.clone()),
        _ => Err(anyhow!(
            "cwd ({}) matches multiple agents — this is a config bug \
             (workdirs should be unique). Pass --as <slug> to disambiguate.",
            cwd.display(),
        )),
    }
}

/// Locate the most-recent CLI session log for `runtime` that
/// corresponds to `workdir`. Best-effort: returns None if the runtime
/// doesn't keep per-cwd logs or we can't find the conventional path.
pub(crate) fn locate_session_file(runtime: Runtime, workdir: &Path) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().map(PathBuf::from)?;
    match runtime {
        Runtime::Claude => locate_claude_session(&home, workdir),
        Runtime::Agy => locate_agy_session(&home, workdir),
        Runtime::Codex => locate_codex_session(&home, workdir),
    }
}

/// Claude Code stores sessions under `~/.claude/projects/<encoded>/`
/// where `<encoded>` is the workdir absolute path with BOTH `/` and
/// `.` replaced by `-` (verified empirically against
/// `/home/alice/.giga/configs/.../giga`, which Claude encodes as
/// `-home-alice--giga-configs-...-giga` — the leading `/` becomes
/// a leading `-`, and the `.` in `.giga` becomes the second `-` of
/// the `--giga` sequence). Each session is one `<uuid>.jsonl`. We
/// return the most-recently-modified file under that dir.
fn locate_claude_session(home: &Path, workdir: &Path) -> Option<PathBuf> {
    let canon = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let encoded: String = canon
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    let dir = home.join(".claude").join("projects").join(&encoded);
    most_recent_jsonl(&dir)
}

/// Agy (Antigravity / Gemini CLI) keeps a global rolling history at
/// `~/.gemini/antigravity-cli/history.jsonl`. There's no per-workdir
/// subdir today (verified against the coder agent's session on
/// 2026-06-03). We point at the global file; the new agent can grep
/// for cwd-relevant lines.
fn locate_agy_session(home: &Path, _workdir: &Path) -> Option<PathBuf> {
    let p = home
        .join(".gemini")
        .join("antigravity-cli")
        .join("history.jsonl");
    p.exists().then_some(p)
}

/// Codex stores sessions under (best-effort guesses) `~/.codex/sessions/`
/// or `~/.codex/projects/<encoded>/`. We return the most-recent file in
/// whichever exists. The exact convention may need correction as Codex
/// evolves.
fn locate_codex_session(home: &Path, workdir: &Path) -> Option<PathBuf> {
    let canon = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let encoded = canon.to_string_lossy().replace('/', "-");
    for candidate in [
        home.join(".codex").join("projects").join(&encoded),
        home.join(".codex").join("sessions"),
    ] {
        if let Some(p) = most_recent_jsonl(&candidate) {
            return Some(p);
        }
    }
    None
}

/// Return the most-recently-modified `*.jsonl` under `dir`, or None
/// if the dir doesn't exist or contains no jsonl files.
fn most_recent_jsonl(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, p));
        }
    }
    best.map(|(_, p)| p)
}

/// Edit the canonical TOML in-place: set `[[agents]]` where name=slug
/// to `runtime = <new>`. Preserves comments + formatting via
/// `toml_edit`. Modeled on `teleport::update_toml_agent_host`.
fn update_agent_runtime_in_toml(config: &Path, slug: &str, new_runtime: Runtime) -> Result<()> {
    let original =
        fs::read_to_string(config).with_context(|| format!("reading {}", config.display()))?;
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
            if name == slug {
                entry["runtime"] = value(new_runtime.as_str());
                updated = true;
                break;
            }
        }
    }
    if !updated {
        return Err(anyhow!(
            "agent `{slug}` not found in [[agents]] (TOML may have been edited concurrently)"
        ));
    }
    fs::write(config, doc.to_string()).with_context(|| format!("writing {}", config.display()))?;
    Ok(())
}

/// Render the markdown block prepended to HANDOVER.md so the new
/// agent reads it on session start (intros tell every runtime to read
/// HANDOVER.md from cwd).
pub(crate) fn render_takeover_block(
    slug: &str,
    old_runtime: Runtime,
    new_runtime: Runtime,
    session_path: Option<&Path>,
) -> String {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let session_line = match session_path {
        Some(p) => format!(
            "> Prior `{}` session log: `{}`. Read recent entries (tail \
             or filter to your workdir) to ingest what the previous \
             agent was doing before you took over.\n",
            old_runtime.as_str(),
            p.display(),
        ),
        None => format!(
            "> Prior `{}` session log: not found on this host. \
             Proceed from whatever channel history + HANDOVER content \
             is visible.\n",
            old_runtime.as_str(),
        ),
    };
    format!(
        "> **TAKEOVER ({ts}) — runtime flipped `{old}` -> `{new}`.**\n\
         >\n\
         > This workdir was previously running as a `{old}` agent (slug \
         `{slug}`). The runtime has been retargeted to `{new}`; \
         AGENTS.md has been regenerated with `{new}`-specific session-\
         start instructions (watcher arming, posting conventions, etc).\n\
         >\n\
         {session_line}>\n\
         > Slug, role, and channel memberships are UNCHANGED — only \
         the runtime flipped. Follow the (now `{new}`-flavored) Session \
         Start protocol in `./AGENTS.md` from scratch.\n\
         \n",
        ts = ts,
        old = old_runtime.as_str(),
        new = new_runtime.as_str(),
        slug = slug,
        session_line = session_line,
    )
}

/// Atomic prepend: write `prefix` then existing content to a temp
/// file in the same dir and rename over the target. Safe against
/// crashes mid-write.
fn prepend_to_file(path: &Path, prefix: &str) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("mkdir -p {}", dir.display()))?;
    let existing = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };
    let tmp = path.with_extension("md.takeover-tmp");
    fs::write(&tmp, format!("{prefix}{existing}"))
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// The single self-contained prompt the new agent uses to drive its
/// first turn. Printed to stdout at the end of `giga takeover` so the
/// invoking agent can quote it verbatim or follow it as a plan.
pub(crate) fn takeover_prompt(
    slug: &str,
    old_runtime: Runtime,
    new_runtime: Runtime,
    session_path: Option<&Path>,
) -> String {
    let prior = match session_path {
        Some(p) => format!(
            "(2) Read the prior {old} session log at {p}. Tail the last \
             ~200 entries (or grep for this workdir if the log is global) \
             to absorb what the previous agent was doing — recent \
             decisions, in-flight tasks, channels they posted to. ",
            old = old_runtime.as_str(),
            p = p.display(),
        ),
        None => format!(
            "(2) No prior {old} session log was found on this host; \
             rely on HANDOVER.md and any visible channel scrollback to \
             reconstruct what the previous agent was doing. ",
            old = old_runtime.as_str(),
        ),
    };
    format!(
        "You are taking over the `{slug}` agent in this workdir; the \
         runtime just flipped from `{old}` to `{new}`. Do this in \
         order: \
         (1) Read ./HANDOVER.md in cwd — its top block summarizes the \
         takeover and points at the prior session. \
         {prior}\
         (3) Read ./AGENTS.md in cwd — it was just regenerated with \
         `{new}`-specific Session Start instructions (watcher arming, \
         posting conventions). Follow that Session Start protocol from \
         scratch as if you were a fresh `{new}` agent. \
         (4) After arming the watcher, post a one-line intro on each \
         channel you participate in saying you are taking over from \
         the prior `{old}` session, and stand by. \
         EVERY response from now on must start with [{slug}].",
        slug = slug,
        old = old_runtime.as_str(),
        new = new_runtime.as_str(),
        prior = prior,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn locate_claude_session_finds_most_recent_jsonl() {
        let tmp_home = tempfile::TempDir::new().unwrap();
        let workdir = tempfile::TempDir::new().unwrap();
        let canon = workdir.path().canonicalize().unwrap();
        // Claude encodes both `/` AND `.` to `-`. The tmpdir path
        // typically contains neither `.` nor anything weird, but
        // construct the encoding the same way the locator does so
        // the test exercises the actual path resolution.
        let encoded: String = canon
            .to_string_lossy()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        let proj_dir = tmp_home
            .path()
            .join(".claude")
            .join("projects")
            .join(&encoded);
        fs::create_dir_all(&proj_dir).unwrap();
        // Two session files, write second one later so its mtime is
        // newer; the locator should pick it.
        let older = proj_dir.join("aaa.jsonl");
        let newer = proj_dir.join("bbb.jsonl");
        fs::write(&older, "{}\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&newer, "{}\n").unwrap();
        let picked = locate_claude_session(tmp_home.path(), workdir.path()).unwrap();
        assert_eq!(picked, newer);
    }

    /// Regression test for the encoding rule. Tempdirs typically
    /// don't have `.` in the path, so the basic test above can pass
    /// even with a buggy encoder. This one explicitly exercises the
    /// `.` → `-` rule by constructing a workdir under a dotdir.
    ///
    /// v0.6.27: gated to unix-only. Windows TempDirs canonicalize to
    /// the `\\?\` extended-path prefix which has its own normalization
    /// rules — leading `\\?\C:\...\-tmpXXX\-giga\...` doesn't preserve
    /// the dot-prefix the way Linux `/tmp/.../.giga/...` does. The
    /// `.giga` workdir convention is a WSL/Linux artifact anyway; the
    /// underlying encoder doesn't need a Windows code path for it.
    #[cfg(unix)]
    #[test]
    fn locate_claude_session_handles_dotdirs_in_workdir() {
        let tmp_home = tempfile::TempDir::new().unwrap();
        // Build a workdir under a `.giga` subdir of a tempdir, mirror
        // of the real giga-harness layout.
        let parent = tempfile::TempDir::new().unwrap();
        let workdir = parent.path().join(".giga").join("workdirs").join("alice");
        fs::create_dir_all(&workdir).unwrap();
        let canon = workdir.canonicalize().unwrap();
        let encoded: String = canon
            .to_string_lossy()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        // Must contain the `--giga` double-dash signature (`/.giga` → `--giga`).
        assert!(
            encoded.contains("--giga"),
            "encoding lost `.` -> `-`: {encoded}"
        );
        let proj_dir = tmp_home
            .path()
            .join(".claude")
            .join("projects")
            .join(&encoded);
        fs::create_dir_all(&proj_dir).unwrap();
        let session = proj_dir.join("x.jsonl");
        fs::write(&session, "{}\n").unwrap();
        let picked = locate_claude_session(tmp_home.path(), &workdir).unwrap();
        assert_eq!(picked, session);
    }

    #[test]
    fn locate_claude_session_returns_none_when_no_jsonl() {
        let tmp_home = tempfile::TempDir::new().unwrap();
        let workdir = tempfile::TempDir::new().unwrap();
        // No ~/.claude/projects/<encoded>/ created at all.
        assert!(locate_claude_session(tmp_home.path(), workdir.path()).is_none());
    }

    #[test]
    fn locate_agy_session_finds_global_history() {
        let tmp_home = tempfile::TempDir::new().unwrap();
        let workdir = tempfile::TempDir::new().unwrap();
        let agy_dir = tmp_home.path().join(".gemini").join("antigravity-cli");
        fs::create_dir_all(&agy_dir).unwrap();
        let hist = agy_dir.join("history.jsonl");
        let mut f = fs::File::create(&hist).unwrap();
        writeln!(f, r#"{{"event":"hi"}}"#).unwrap();
        let picked = locate_agy_session(tmp_home.path(), workdir.path()).unwrap();
        assert_eq!(picked, hist);
    }

    #[test]
    fn update_agent_runtime_preserves_comments() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        let original = "# top comment\n\
                       [project]\n\
                       name = \"t\"\n\
                       \n\
                       [paths]\n\
                       wsl_inbox = \"/tmp/i\"\n\
                       \n\
                       # before agent\n\
                       [[agents]]\n\
                       name = \"alice\"\n\
                       workdir = \"/h/alice\"\n\
                       role = \"r\"\n\
                       platform = \"wsl\"\n\
                       runtime = \"agy\"\n";
        fs::write(&cfg_path, original).unwrap();
        update_agent_runtime_in_toml(&cfg_path, "alice", Runtime::Claude).unwrap();
        let after = fs::read_to_string(&cfg_path).unwrap();
        assert!(
            after.contains("# top comment"),
            "lost top comment:\n{after}"
        );
        assert!(
            after.contains("# before agent"),
            "lost mid comment:\n{after}"
        );
        assert!(
            after.contains("runtime = \"claude\""),
            "runtime not flipped to claude:\n{after}",
        );
        assert!(
            !after.contains("runtime = \"agy\""),
            "old runtime still present:\n{after}",
        );
    }

    #[test]
    fn update_agent_runtime_errors_when_slug_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        let original = "[project]\nname = \"t\"\n\
                       [paths]\nwsl_inbox = \"/tmp/i\"\n\
                       [[agents]]\nname = \"alice\"\nworkdir = \"/h/alice\"\n\
                       role = \"r\"\nplatform = \"wsl\"\n";
        fs::write(&cfg_path, original).unwrap();
        let err = update_agent_runtime_in_toml(&cfg_path, "bob", Runtime::Claude).unwrap_err();
        assert!(format!("{err}").contains("not found"), "{err}");
    }

    #[test]
    fn prepend_to_file_creates_when_missing_and_prepends_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("HANDOVER.md");
        // First call: file doesn't exist → just writes the prefix.
        prepend_to_file(&p, "PREFIX1\n").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "PREFIX1\n");
        // Second call: prefix goes on TOP of existing content.
        prepend_to_file(&p, "PREFIX2\n").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "PREFIX2\nPREFIX1\n");
    }

    #[test]
    fn takeover_prompt_includes_runtime_swap_and_handover_pointer() {
        let prompt = takeover_prompt("coder", Runtime::Agy, Runtime::Claude, None);
        assert!(prompt.contains("coder"), "missing slug:\n{prompt}");
        assert!(prompt.contains("agy"), "missing old runtime:\n{prompt}");
        assert!(prompt.contains("claude"), "missing new runtime:\n{prompt}");
        assert!(
            prompt.contains("HANDOVER.md"),
            "missing handover ref:\n{prompt}"
        );
        assert!(
            prompt.contains("AGENTS.md"),
            "missing AGENTS.md ref:\n{prompt}"
        );
        assert!(
            prompt.contains("[coder]"),
            "missing reply-prefix rule:\n{prompt}"
        );
    }

    #[test]
    fn takeover_prompt_mentions_session_path_when_present() {
        let path = Path::new("/home/x/.claude/projects/abc/zz.jsonl");
        let prompt = takeover_prompt("coder", Runtime::Agy, Runtime::Claude, Some(path));
        assert!(
            prompt.contains("/home/x/.claude/projects/abc/zz.jsonl"),
            "session path not surfaced in prompt:\n{prompt}",
        );
    }

    #[test]
    fn takeover_prompt_handles_missing_session_gracefully() {
        let prompt = takeover_prompt("coder", Runtime::Codex, Runtime::Claude, None);
        assert!(
            prompt.to_lowercase().contains("no prior") || prompt.contains("not found"),
            "missing-session prompt should call that out:\n{prompt}",
        );
    }

    #[test]
    fn render_takeover_block_labels_both_runtimes_and_points_at_agents_md() {
        let block = render_takeover_block("coder", Runtime::Agy, Runtime::Claude, None);
        assert!(
            block.contains("TAKEOVER"),
            "missing TAKEOVER header:\n{block}"
        );
        assert!(block.contains("agy"), "old runtime missing:\n{block}");
        assert!(block.contains("claude"), "new runtime missing:\n{block}");
        assert!(
            block.contains("AGENTS.md"),
            "AGENTS.md pointer missing:\n{block}"
        );
    }
}
