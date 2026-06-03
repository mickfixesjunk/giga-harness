//! `giga add-agent` — scaffold a new agent into an existing project.
//!
//! Appends `[[agents]]` + per-peer `[[channels]]` blocks to the
//! canonical TOML config (preserving comments + formatting via
//! `toml_edit`), appends the new slug to any broadcast-channel
//! participants list (channels whose `file` starts with `_`),
//! writes a minimal `agents/<slug>.md` template, and re-validates
//! the result before returning.
//!
//! Intended to be runnable from any swarm agent's session so they
//! can add new agents on the user's behalf without hand-editing TOML.
//! Launch is a separate step the user owns (window-layout intent
//! is theirs).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

use crate::config::Config;
use crate::sync;

pub struct Args {
    pub config: PathBuf,
    pub name: String,
    pub workdir: String,
    pub role: String,
    pub platform: String,
    pub peers: Vec<String>,
    pub bench_scheduler: bool,
    pub no_broadcast: bool,
    pub template: Option<PathBuf>,
    pub dry_run: bool,
    pub code_root: Option<String>,
    /// Optional host name (must match a `[[hosts]].name`) — set when
    /// scaffolding an agent that lives on a peer host. The TOML
    /// `[[agents]]` entry will carry `host = "..."`; sync (step 5)
    /// ships the canonical TOML to peers so they learn about it. The
    /// user then runs `giga launch --host <peer> --only <new-agent>`
    /// (or `giga remote --host <peer> launch --only <new-agent>`) to
    /// actually bring up the terminal on the peer.
    pub host: Option<String>,
}

pub fn run(args: Args) -> Result<()> {
    // ---- pre-flight against the parsed config -----------------------
    let cfg = Config::load(&args.config)?;
    preflight(&cfg, &args)?;

    // ---- edit the TOML doc in memory -------------------------------
    let original = fs::read_to_string(&args.config)
        .with_context(|| format!("reading {}", args.config.display()))?;
    let mut doc: DocumentMut = original
        .parse()
        .with_context(|| format!("parsing {} as TOML", args.config.display()))?;

    let new_channels = derive_channels(&cfg, &args);
    let broadcast_targets = if args.no_broadcast {
        Vec::new()
    } else {
        find_broadcast_channels(&cfg)
    };

    append_agent(&mut doc, &args)?;
    for ch in &new_channels {
        append_channel(&mut doc, ch)?;
    }
    for broadcast_file in &broadcast_targets {
        append_to_broadcast(&mut doc, broadcast_file, &args.name)?;
    }

    let updated = doc.to_string();

    // ---- decide on template path -----------------------------------
    let template_path = template_target(&args.config, &args.name)?;
    let template_body = match &args.template {
        Some(p) => fs::read_to_string(p)
            .with_context(|| format!("reading custom template {}", p.display()))?,
        None => render_template(&args),
    };

    // ---- dry-run short-circuits BEFORE touching disk ---------------
    if args.dry_run {
        println!("dry-run: would add agent `{}`", args.name);
        println!("  - workdir: {}", args.workdir);
        println!("  - platform: {}", args.platform);
        println!("  - role: {}", args.role);
        for ch in &new_channels {
            println!(
                "  - [[channels]] {} ({}, {} ↔ {})",
                ch.file, ch.side, ch.participants[0], ch.participants[1],
            );
        }
        for f in &broadcast_targets {
            println!(
                "  - append `{}` to broadcast participants of {}",
                args.name, f
            );
        }
        println!("  - write template: {}", template_path.display());
        println!("(dry-run — no files modified)");
        return Ok(());
    }

    // ---- write changes ---------------------------------------------
    fs::write(&args.config, &updated)
        .with_context(|| format!("writing updated {}", args.config.display()))?;
    if let Some(parent) = template_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    // v0.3.9 Bug 3: collision check happens in preflight (BEFORE the
    // TOML write). If template_path already exists at this point, the
    // operator passed --template pointing at it — the file IS the
    // template body, no write needed.
    if !template_path.exists() {
        fs::write(&template_path, template_body)
            .with_context(|| format!("writing {}", template_path.display()))?;
    }

    // ---- re-validate the updated config ----------------------------
    let revalidated = Config::load(&args.config)
        .context("re-loading config after edit failed — config is in an unexpected state")?;
    revalidated
        .validate()
        .context("re-validating after edit failed — config is in an unexpected state")?;

    // ---- summary ----------------------------------------------------
    println!(
        "added agent `{}` ({}, {})",
        args.name, args.platform, args.workdir
    );
    for ch in &new_channels {
        println!(
            "  + [[channels]] {} ({}, {} ↔ {})",
            ch.file, ch.side, ch.participants[0], ch.participants[1],
        );
    }
    for f in &broadcast_targets {
        println!(
            "  + appended `{}` to broadcast participants of {}",
            args.name, f
        );
    }
    println!("  + wrote {}", template_path.display());

    // ---- auto-bootstrap peer when --host names a non-local host ------
    // Replaces the runbook's manual "rsync the swarm dir to peer +
    // create this_host.toml" step from REMOTE_QUICKSTART.md. Best-effort:
    // on failure we warn but don't fail the local-side success (the
    // operator can re-run `giga sync` later to recover).
    if let Some(host) = &args.host {
        let is_remote = revalidated.this_host.as_deref() != Some(host.as_str());
        if is_remote {
            println!();
            println!("auto-bootstrap: pushing canonical TOML to `{host}`...");
            let bootstrap_ok = match sync::bootstrap_peer(&revalidated, host, &args.config) {
                Ok(()) => {
                    println!(
                        "  + canonical TOML synced to `{host}` (and this_host.toml ensured)"
                    );
                    true
                }
                Err(e) => {
                    eprintln!("  ! auto-bootstrap failed: {e:#}");
                    eprintln!("    The local config is correct; the peer just isn't synced yet.");
                    eprintln!("    Run `giga sync --once` once everything is reachable to recover.");
                    false
                }
            };
            // Remote `giga init` scaffolds the new agent's workdir +
            // AGENTS.md on the peer. Init is host-aware (as of v1.1), so
            // it only touches workdirs for agents whose `host` matches
            // the peer — won't try to mkdir /home/<other-user>/... on
            // the wrong filesystem. Best-effort: only runs if bootstrap
            // succeeded (otherwise the peer doesn't even have the TOML
            // to init from).
            if bootstrap_ok {
                println!("auto-scaffold: running `giga init` on `{host}`...");
                match sync::run_remote_giga_init(&revalidated, host, &args.config) {
                    Ok(()) => println!("  + remote init complete — `{}`'s workdir + AGENTS.md ready on `{host}`", args.name),
                    Err(e) => {
                        eprintln!("  ! remote giga init failed: {e:#}");
                        eprintln!("    The peer has the TOML; run `giga remote --host {host} init` manually to scaffold.");
                    }
                }
            }
        }
    }

    println!();
    println!("next:");
    println!("  giga validate {}", args.config.display());
    if args.host.is_some() {
        println!(
            "  giga launch --host {} --only {}    # bring up the agent's terminal on the peer",
            args.host.as_deref().unwrap_or(""),
            args.name,
        );
    } else {
        println!("  # if multi-host: re-localize first, then launch from your terminal:");
        println!(
            "  # ./setup-<host>.sh && giga launch --only {} --new-window <localized-config>",
            args.name
        );
    }
    Ok(())
}

// --------------------------------------------------------------- pre-flight

/// Refuse to write a path with a leading tilde to the TOML. Used by both
/// --workdir and --code-root validation. The error message points at the
/// remediation operators most often want (the absolute path that the
/// tilde was trying to spell). Pure — testable.
fn reject_tilde(flag: &str, path: &str) -> Result<()> {
    if path.starts_with('~') {
        let abs_hint = if let Some(home) = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .and_then(|h| h.into_string().ok())
        {
            path.replacen('~', &home, 1)
        } else {
            format!("/home/<user>{}", &path[1..])
        };
        return Err(anyhow!(
            "{flag} contains a literal `~` — this won't expand at `giga launch` time \
             because the path gets shell-escaped before bash sees it.\n\n\
             For local agents on this host, use the absolute path:\n\
             \u{20}\u{20}{flag} {abs_hint}\n\n\
             For cross-host agents (--host <peer>), use the absolute path under THAT host's \
             $HOME (e.g. /home/<their-user>/.giga/configs/<swarm>/workdirs/<slug>) — giga can't \
             auto-expand `~` against a remote host's $HOME from the operator side."
        ));
    }
    Ok(())
}

fn preflight(cfg: &Config, args: &Args) -> Result<()> {
    if args.name.is_empty() {
        return Err(anyhow!("--name cannot be empty"));
    }
    if !args
        .name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(anyhow!(
            "--name `{}` must be kebab-case: lowercase ASCII letters, digits, hyphens only",
            args.name,
        ));
    }
    if cfg.agents.iter().any(|a| a.name == args.name) {
        return Err(anyhow!("agent `{}` already exists in config", args.name));
    }
    if args.workdir.is_empty() {
        return Err(anyhow!("--workdir cannot be empty"));
    }
    // Reject literal `~` early — see quality finding 3 (2026-05-31): the
    // path is shell_escape::escape'd into the launch `cd` command, which
    // single-quotes `~` and bash doesn't expand it. Fails loudly at TOML
    // write time instead of confusingly hours later in a tmux pane.
    // Cross-host case: we don't know the peer's $HOME locally, so we
    // can't auto-expand. Local case: we could expand via dirs::home_dir()
    // but absolute paths make the TOML more portable across machines, so
    // a hard rule is cleaner.
    reject_tilde("--workdir", &args.workdir)?;
    if let Some(cr) = &args.code_root {
        reject_tilde("--code-root", cr)?;
    }
    if args.role.is_empty() {
        return Err(anyhow!("--role cannot be empty"));
    }
    if args.platform != "wsl" && args.platform != "windows" {
        return Err(anyhow!(
            "--platform must be `wsl` or `windows`, got `{}`",
            args.platform,
        ));
    }
    if args.bench_scheduler {
        let existing = cfg.agents.iter().any(|a| a.bench_scheduler);
        if existing {
            return Err(anyhow!(
                "another agent is already the bench scheduler — only one per project",
            ));
        }
    }
    let known: HashSet<&str> = cfg.agents.iter().map(|a| a.name.as_str()).collect();
    for peer in &args.peers {
        if peer == &args.name {
            return Err(anyhow!("--peer `{}` is the agent itself", peer));
        }
        if !known.contains(peer.as_str()) {
            return Err(anyhow!(
                "--peer `{}` is not a known agent. Known: {}",
                peer,
                cfg.agents
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
    }
    // v0.3.9 Bug 3: template-exists check moved here (was post-TOML-write
    // → half-committed state on failure). Special-case the "user passes
    // --template pointing at the target itself" case: that's intentional
    // pre-write usage, not a collision. Tested via canonicalize so
    // relative vs absolute paths compare correctly.
    //
    // When the config path has no parent (test fixtures that pass
    // PathBuf::new()), skip the existence check — there's no real
    // filesystem target to check against.
    let template_path = match template_target(&args.config, &args.name) {
        Ok(p) => p,
        Err(_) => return Ok(()), // no parent dir; nothing to check
    };
    if template_path.exists() {
        let in_place = match &args.template {
            Some(p) => {
                let a = std::fs::canonicalize(p).ok();
                let b = std::fs::canonicalize(&template_path).ok();
                a.is_some() && a == b
            }
            None => false,
        };
        if !in_place {
            return Err(anyhow!(
                "template path {} already exists; refusing to overwrite. \
                 Either remove it manually if it's a leftover, pick a \
                 different --name, or pass --template {} to use the \
                 existing file in place.",
                template_path.display(),
                template_path.display(),
            ));
        }
    }
    Ok(())
}

// --------------------------------------------------------------- channel derivation

#[derive(Debug)]
pub struct DerivedChannel {
    pub file: String,
    pub side: String,
    pub participants: [String; 2],
    pub purpose: String,
}

fn derive_channels(cfg: &Config, args: &Args) -> Vec<DerivedChannel> {
    args.peers
        .iter()
        .map(|peer| {
            // Alphabetical filename: predictable, easy to find on disk.
            let mut both = vec![args.name.clone(), peer.clone()];
            both.sort();
            let file = format!("{}-{}.md", both[0], both[1]);

            // Side: if either participant is windows-platform, the
            // channel must live on the windows side so the native
            // Windows agent can reach it (WSL agents can read /mnt/c
            // either way).
            let peer_platform = cfg
                .agents
                .iter()
                .find(|a| &a.name == peer)
                .map(|a| a.platform.as_str())
                .unwrap_or("wsl");
            let side = if args.platform == "windows" || peer_platform == "windows" {
                "windows"
            } else {
                "wsl"
            }
            .to_string();

            DerivedChannel {
                file,
                side,
                participants: [both[0].clone(), both[1].clone()],
                purpose: format!("Bilateral channel between {} and {}.", both[0], both[1]),
            }
        })
        .collect()
}

fn find_broadcast_channels(cfg: &Config) -> Vec<String> {
    cfg.channels
        .iter()
        .filter(|c| c.file.starts_with('_'))
        .map(|c| c.file.clone())
        .collect()
}

// --------------------------------------------------------------- toml_edit helpers

fn append_agent(doc: &mut DocumentMut, args: &Args) -> Result<()> {
    let agents = ensure_array_of_tables(doc, "agents")?;
    let mut block = Table::new();
    block["name"] = value(args.name.as_str());
    block["workdir"] = value(args.workdir.as_str());
    block["role"] = value(args.role.as_str());
    block["platform"] = value(args.platform.as_str());
    if let Some(h) = &args.host {
        block["host"] = value(h.as_str());
    }
    if args.bench_scheduler {
        block["bench_scheduler"] = value(true);
    }
    if let Some(cr) = &args.code_root {
        block["code_root"] = value(cr.as_str());
    }
    block["claudemd_template"] = value(format!("agents/{}.md", args.name));
    agents.push(block);
    Ok(())
}

pub(crate) fn append_channel(doc: &mut DocumentMut, ch: &DerivedChannel) -> Result<()> {
    let channels = ensure_array_of_tables(doc, "channels")?;
    let mut block = Table::new();
    block["file"] = value(ch.file.as_str());
    block["side"] = value(ch.side.as_str());
    let mut participants = Array::new();
    participants.push(ch.participants[0].as_str());
    participants.push(ch.participants[1].as_str());
    block["participants"] = value(participants);
    block["purpose"] = value(ch.purpose.as_str());
    channels.push(block);
    Ok(())
}

fn append_to_broadcast(doc: &mut DocumentMut, file: &str, slug: &str) -> Result<()> {
    let channels = doc
        .get_mut("channels")
        .and_then(|i| i.as_array_of_tables_mut())
        .ok_or_else(|| anyhow!("config has no [[channels]] section"))?;
    for table in channels.iter_mut() {
        let f = table.get("file").and_then(|v| v.as_str()).unwrap_or("");
        if f != file {
            continue;
        }
        let participants = table
            .get_mut("participants")
            .and_then(|i| i.as_array_mut())
            .ok_or_else(|| anyhow!("broadcast channel `{}` has no participants array", file))?;
        // Idempotency guard: if slug is already in the list, do nothing.
        let already_present = participants
            .iter()
            .any(|v| v.as_str().map(|s| s == slug).unwrap_or(false));
        if !already_present {
            participants.push(slug);
        }
        return Ok(());
    }
    Err(anyhow!(
        "broadcast channel `{}` not found in [[channels]]",
        file
    ))
}

pub(crate) fn ensure_array_of_tables<'a>(
    doc: &'a mut DocumentMut,
    key: &str,
) -> Result<&'a mut ArrayOfTables> {
    if !doc.contains_key(key) {
        doc.insert(key, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    doc.get_mut(key)
        .and_then(|i| i.as_array_of_tables_mut())
        .ok_or_else(|| anyhow!("config key `{}` exists but is not an array of tables", key))
}

// --------------------------------------------------------------- template

fn template_target(config_path: &Path, name: &str) -> Result<PathBuf> {
    let dir = config_path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent dir"))?;
    Ok(dir.join("agents").join(format!("{name}.md")))
}

fn render_template(args: &Args) -> String {
    let peer_list = if args.peers.is_empty() {
        "(no bilateral peers yet — coordinate only via broadcast)".to_string()
    } else {
        args.peers
            .iter()
            .map(|p| format!("`{p}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // Templates live in-repo under `templates/` and are compiled into the
    // binary with `include_str!` — no runtime dependency on any external
    // configs repo. Shared prose (watcher arming, message convention) lives
    // in `templates/partials/` so init/add-agent stay in sync.
    let watcher = crate::templates::WATCHER.replace("{{AGENT}}", &args.name);
    crate::templates::AGENT_STUB
        .replace("{{WATCHER}}", watcher.trim_end())
        .replace("{{CONVENTION}}", crate::templates::CONVENTION.trim_end())
        .replace("{{PEERS}}", &peer_list)
        .replace("{{ROLE}}", &args.role)
        .replace("{{AGENT}}", &args.name)
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tempfile::TempDir;

    fn write_config(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("giga-harness.toml");
        fs::write(&path, body).unwrap();
        path
    }

    fn minimal_config_text() -> &'static str {
        r#"
[project]
name = "testproj"

[paths]
wsl_inbox = "/tmp/inbox"

[[agents]]
name = "alice"
workdir = "/home/me/alice"
role = "Implementation."
platform = "wsl"
claudemd_template = "agents/alice.md"

[[agents]]
name = "bob"
workdir = "/home/me/bob"
role = "Review."
platform = "wsl"
claudemd_template = "agents/bob.md"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
purpose = "Implementation ↔ review."
"#
    }

    fn config_with_broadcast_text() -> &'static str {
        r#"
[project]
name = "testproj"

[paths]
wsl_inbox = "/tmp/inbox"
windows_inbox = "/tmp/inbox_win"

[[agents]]
name = "alice"
workdir = "/home/me/alice"
role = "Implementation."
platform = "wsl"
claudemd_template = "agents/alice.md"

[[agents]]
name = "bob"
workdir = "/home/me/bob"
role = "Review."
platform = "wsl"
claudemd_template = "agents/bob.md"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
purpose = "Implementation ↔ review."

[[channels]]
file = "_broadcast.md"
side = "windows"
participants = ["alice", "bob"]
purpose = "All-hands."
"#
    }

    fn base_args(config: PathBuf) -> Args {
        Args {
            config,
            name: "charlie".into(),
            workdir: "/home/me/charlie".into(),
            role: "Testing.".into(),
            platform: "wsl".into(),
            peers: vec!["alice".into()],
            bench_scheduler: false,
            no_broadcast: false,
            template: None,
            dry_run: false,
            code_root: None,
            host: None,
        }
    }

    // ----- preflight --------------------------------------------------

    #[test]
    fn preflight_rejects_empty_name() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.name = "".into();
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("--name cannot be empty"));
    }

    #[test]
    fn preflight_rejects_caps_in_slug() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.name = "Alice".into();
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("kebab-case"));
    }

    #[test]
    fn preflight_rejects_space_in_slug() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.name = "my agent".into();
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("kebab-case"));
    }

    #[test]
    fn preflight_rejects_duplicate_slug() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.name = "alice".into();
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    // -----------------------------------------------------------------
    // v0.3.3: reject `~` in --workdir / --code-root early (quality finding 3).
    // -----------------------------------------------------------------

    #[test]
    fn preflight_rejects_tilde_workdir() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.workdir = "~/.giga/configs/x/workdirs/y".into();
        let err = preflight(&cfg, &args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--workdir"));
        assert!(msg.contains("literal `~`"));
        // Surfaces remediation:
        assert!(msg.contains("absolute path"));
    }

    #[test]
    fn preflight_rejects_tilde_code_root() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.code_root = Some("~/projects/myproj".into());
        let err = preflight(&cfg, &args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--code-root"));
        assert!(msg.contains("literal `~`"));
    }

    #[test]
    fn preflight_accepts_absolute_workdir() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.workdir = "/home/me/.giga/configs/x/workdirs/y".into();
        assert!(preflight(&cfg, &args).is_ok());
    }

    #[test]
    fn reject_tilde_inlines_home_in_remediation_when_HOME_set() {
        // Save + restore HOME so we don't break other tests.
        let orig = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", "/home/quality-test") };
        let err = reject_tilde("--workdir", "~/x").unwrap_err();
        match orig {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        assert!(err.to_string().contains("/home/quality-test/x"));
    }

    #[test]
    fn preflight_rejects_empty_role() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.role = "".into();
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("--role cannot be empty"));
    }

    #[test]
    fn preflight_rejects_unknown_peer() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.peers = vec!["nope".into()];
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("not a known agent"));
    }

    #[test]
    fn preflight_rejects_self_peer() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.peers = vec!["charlie".into()];
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("agent itself"));
    }

    #[test]
    fn preflight_rejects_bad_platform() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.platform = "macos".into();
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("--platform must be"));
    }

    #[test]
    fn preflight_rejects_second_bench_scheduler() {
        let body = format!(
            "{}\nbench_scheduler = true\n",
            minimal_config_text().replace(
                r#"name = "alice""#,
                r#"name = "alice"
bench_scheduler = true"#,
            )
        );
        let cfg: Config = Config::load_str_for_test(&body).unwrap();
        let mut args = base_args(PathBuf::new());
        args.bench_scheduler = true;
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("bench scheduler"));
    }

    #[test]
    fn preflight_accepts_minimal_valid() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let args = base_args(PathBuf::new());
        preflight(&cfg, &args).unwrap();
    }

    /// v0.3.9 Bug 3: when a stray `agents/<slug>.md` exists from a
    /// prior aborted add, preflight catches it BEFORE any TOML edit.
    /// Pre-fix: TOML got appended with the new agent + channels +
    /// broadcast participants, then the template-write step errored
    /// with the file-exists message. Operator left holding a
    /// half-committed config.
    #[test]
    fn preflight_rejects_when_template_exists_and_not_in_place() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg_path, minimal_config_text()).unwrap();
        std::fs::create_dir_all(tmp.path().join("agents")).unwrap();
        std::fs::write(
            tmp.path().join("agents").join("charlie.md"),
            "# stray template\n",
        )
        .unwrap();
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let args = base_args(cfg_path);
        let err = preflight(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert!(err.to_string().contains("--template"));
    }

    /// v0.3.9 Bug 3: when --template points at the template_target
    /// path itself, preflight allows it (user pre-wrote the template
    /// and is telling us to use it in place).
    #[test]
    fn preflight_accepts_template_pointing_at_target_in_place() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg_path, minimal_config_text()).unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        let template_path = agents_dir.join("charlie.md");
        std::fs::write(&template_path, "# pre-written body\n").unwrap();
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(cfg_path);
        args.template = Some(template_path);
        // Preflight must accept this — user passed --template pointing
        // exactly at where add-agent would write.
        preflight(&cfg, &args).unwrap();
    }

    // ----- channel derivation -----------------------------------------

    #[test]
    fn derives_alphabetical_filename() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let args = base_args(PathBuf::new());
        let channels = derive_channels(&cfg, &args);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].file, "alice-charlie.md");
        assert_eq!(
            channels[0].participants,
            ["alice".to_string(), "charlie".to_string()]
        );
    }

    #[test]
    fn derives_windows_side_when_peer_is_windows() {
        let body = minimal_config_text().replace(
            r#"name = "bob"
workdir = "/home/me/bob"
role = "Review."
platform = "wsl""#,
            r#"name = "bob"
workdir = "C:\\Users\\me\\bob"
role = "Review."
platform = "windows""#,
        );
        // Need windows_inbox for windows-side channels to validate later.
        let body = body.replace(
            r#"[paths]
wsl_inbox = "/tmp/inbox""#,
            r#"[paths]
wsl_inbox = "/tmp/inbox"
windows_inbox = "/tmp/inbox_win""#,
        );
        let cfg = Config::load_str_for_test(&body).unwrap();
        let mut args = base_args(PathBuf::new());
        args.peers = vec!["bob".into()];
        let channels = derive_channels(&cfg, &args);
        assert_eq!(channels[0].side, "windows");
    }

    #[test]
    fn derives_wsl_side_for_two_wsl_agents() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let args = base_args(PathBuf::new());
        let channels = derive_channels(&cfg, &args);
        assert_eq!(channels[0].side, "wsl");
    }

    #[test]
    fn derives_one_channel_per_peer() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let mut args = base_args(PathBuf::new());
        args.peers = vec!["alice".into(), "bob".into()];
        let channels = derive_channels(&cfg, &args);
        assert_eq!(channels.len(), 2);
        let files: Vec<&str> = channels.iter().map(|c| c.file.as_str()).collect();
        assert!(files.contains(&"alice-charlie.md"));
        assert!(files.contains(&"bob-charlie.md"));
    }

    // ----- broadcast detection ----------------------------------------

    #[test]
    fn finds_broadcast_channel_by_underscore_prefix() {
        let cfg = Config::load_str_for_test(config_with_broadcast_text()).unwrap();
        let found = find_broadcast_channels(&cfg);
        assert_eq!(found, vec!["_broadcast.md".to_string()]);
    }

    #[test]
    fn ignores_non_broadcast_channels() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let found = find_broadcast_channels(&cfg);
        assert!(found.is_empty());
    }

    // ----- toml editing -----------------------------------------------

    #[test]
    fn append_agent_preserves_other_content() {
        let mut doc: DocumentMut = minimal_config_text().parse().unwrap();
        let args = base_args(PathBuf::new());
        append_agent(&mut doc, &args).unwrap();
        let out = doc.to_string();
        // Existing agents survive intact:
        assert!(out.contains(r#"name = "alice""#));
        assert!(out.contains(r#"name = "bob""#));
        // New one appended:
        assert!(out.contains(r#"name = "charlie""#));
        assert!(out.contains(r#"workdir = "/home/me/charlie""#));
        // claudemd_template auto-set:
        assert!(out.contains(r#"claudemd_template = "agents/charlie.md""#));
    }

    #[test]
    fn append_agent_with_bench_scheduler_sets_field() {
        let mut doc: DocumentMut = minimal_config_text().parse().unwrap();
        let mut args = base_args(PathBuf::new());
        args.bench_scheduler = true;
        append_agent(&mut doc, &args).unwrap();
        let out = doc.to_string();
        assert!(out.contains("bench_scheduler = true"));
    }

    #[test]
    fn append_agent_without_bench_scheduler_omits_field() {
        let mut doc: DocumentMut = minimal_config_text().parse().unwrap();
        let args = base_args(PathBuf::new());
        append_agent(&mut doc, &args).unwrap();
        // Find the charlie block and check it doesn't have bench_scheduler.
        let out = doc.to_string();
        let charlie_section = out.split(r#"name = "charlie""#).nth(1).unwrap();
        // Section continues until next [[ or end. bench_scheduler must
        // not appear within this section.
        let cut = charlie_section.find("[[").unwrap_or(charlie_section.len());
        assert!(!charlie_section[..cut].contains("bench_scheduler"));
    }

    #[test]
    fn append_agent_with_code_root_emits_field() {
        let mut doc: DocumentMut = minimal_config_text().parse().unwrap();
        let mut args = base_args(PathBuf::new());
        args.code_root = Some("/code/myproj".to_string());
        append_agent(&mut doc, &args).unwrap();
        let out = doc.to_string();
        assert!(
            out.contains(r#"code_root = "/code/myproj""#),
            "TOML output missing code_root field. Full output:\n{}",
            out,
        );
    }

    #[test]
    fn append_agent_without_code_root_omits_field() {
        let mut doc: DocumentMut = minimal_config_text().parse().unwrap();
        let args = base_args(PathBuf::new()); // code_root: None
        append_agent(&mut doc, &args).unwrap();
        let out = doc.to_string();
        let charlie_section = out.split(r#"name = "charlie""#).nth(1).unwrap();
        let cut = charlie_section.find("[[").unwrap_or(charlie_section.len());
        assert!(
            !charlie_section[..cut].contains("code_root"),
            "code_root should not appear when not set",
        );
    }

    #[test]
    fn end_to_end_code_root_survives_reload() {
        // After add-agent runs, Config::load on the updated TOML must
        // see the code_root field on the new agent.
        let tmp = TempDir::new().unwrap();
        let cfg_path = write_config(tmp.path(), minimal_config_text());
        let mut args = base_args(cfg_path.clone());
        args.code_root = Some("/code/myproj".to_string());
        run(args).unwrap();
        let reloaded = Config::load(&cfg_path).unwrap();
        let charlie = reloaded
            .agents
            .iter()
            .find(|a| a.name == "charlie")
            .expect("charlie should exist");
        assert_eq!(
            charlie
                .code_root
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            Some("/code/myproj".to_string()),
        );
    }

    #[test]
    fn append_channel_writes_complete_block() {
        let mut doc: DocumentMut = minimal_config_text().parse().unwrap();
        let ch = DerivedChannel {
            file: "alice-charlie.md".into(),
            side: "wsl".into(),
            participants: ["alice".into(), "charlie".into()],
            purpose: "test".into(),
        };
        append_channel(&mut doc, &ch).unwrap();
        let out = doc.to_string();
        assert!(out.contains(r#"file = "alice-charlie.md""#));
        assert!(out.contains(r#"side = "wsl""#));
        assert!(out.contains(r#"participants = ["alice", "charlie"]"#));
    }

    #[test]
    fn append_to_broadcast_adds_participant() {
        let mut doc: DocumentMut = config_with_broadcast_text().parse().unwrap();
        append_to_broadcast(&mut doc, "_broadcast.md", "charlie").unwrap();
        let out = doc.to_string();
        // The participants line for _broadcast.md should now include charlie.
        let broadcast_section = out.split(r#"file = "_broadcast.md""#).nth(1).unwrap();
        let participants_line = broadcast_section
            .lines()
            .find(|l| l.contains("participants ="))
            .unwrap();
        assert!(participants_line.contains("charlie"));
    }

    #[test]
    fn append_to_broadcast_is_idempotent() {
        let mut doc: DocumentMut = config_with_broadcast_text().parse().unwrap();
        append_to_broadcast(&mut doc, "_broadcast.md", "alice").unwrap();
        let out = doc.to_string();
        // Alice shouldn't be duplicated.
        let broadcast_section = out
            .split(r#"file = "_broadcast.md""#)
            .nth(1)
            .unwrap()
            .split("[[")
            .next()
            .unwrap();
        let alice_count = broadcast_section.matches(r#""alice""#).count();
        assert_eq!(
            alice_count, 1,
            "alice should appear exactly once, got {}",
            alice_count
        );
    }

    #[test]
    fn append_to_broadcast_errors_when_missing() {
        let mut doc: DocumentMut = minimal_config_text().parse().unwrap();
        let err = append_to_broadcast(&mut doc, "_broadcast.md", "charlie").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    // ----- end-to-end via run() ---------------------------------------

    #[test]
    fn end_to_end_adds_agent_channel_template() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = write_config(tmp.path(), minimal_config_text());
        let args = Args {
            config: cfg_path.clone(),
            name: "charlie".into(),
            workdir: "/home/me/charlie".into(),
            role: "Testing.".into(),
            platform: "wsl".into(),
            peers: vec!["alice".into()],
            bench_scheduler: false,
            no_broadcast: false,
            template: None,
            dry_run: false,
            code_root: None,
            host: None,
        };
        run(args).unwrap();

        // Config has charlie + new channel:
        let updated = fs::read_to_string(&cfg_path).unwrap();
        assert!(updated.contains(r#"name = "charlie""#));
        assert!(updated.contains(r#"file = "alice-charlie.md""#));

        // Template was written:
        let tpl = tmp.path().join("agents").join("charlie.md");
        assert!(tpl.exists(), "template not created");
        let tpl_body = fs::read_to_string(&tpl).unwrap();
        assert!(tpl_body.contains("# charlie agent"));
        assert!(tpl_body.contains("giga watch --as charlie"));

        // Re-load + validate via the library:
        let cfg = Config::load(&cfg_path).unwrap();
        assert!(cfg.agents.iter().any(|a| a.name == "charlie"));
        assert!(cfg.channels.iter().any(|c| c.file == "alice-charlie.md"));
    }

    #[test]
    fn end_to_end_dry_run_does_not_touch_disk() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = write_config(tmp.path(), minimal_config_text());
        let original = fs::read_to_string(&cfg_path).unwrap();
        let args = Args {
            config: cfg_path.clone(),
            name: "charlie".into(),
            workdir: "/home/me/charlie".into(),
            role: "Testing.".into(),
            platform: "wsl".into(),
            peers: vec!["alice".into()],
            bench_scheduler: false,
            no_broadcast: false,
            template: None,
            dry_run: true,
            code_root: None,
            host: None,
        };
        run(args).unwrap();
        let after = fs::read_to_string(&cfg_path).unwrap();
        assert_eq!(original, after, "dry-run modified config");
        assert!(
            !tmp.path().join("agents").exists(),
            "dry-run created agents dir"
        );
    }

    #[test]
    fn end_to_end_appends_broadcast_when_present() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = write_config(tmp.path(), config_with_broadcast_text());
        let args = Args {
            config: cfg_path.clone(),
            name: "charlie".into(),
            workdir: "/home/me/charlie".into(),
            role: "Testing.".into(),
            platform: "wsl".into(),
            peers: vec!["alice".into()],
            bench_scheduler: false,
            no_broadcast: false,
            template: None,
            dry_run: false,
            code_root: None,
            host: None,
        };
        run(args).unwrap();
        let updated = fs::read_to_string(&cfg_path).unwrap();
        let bsec = updated.split(r#"file = "_broadcast.md""#).nth(1).unwrap();
        let p_line = bsec.lines().find(|l| l.contains("participants =")).unwrap();
        assert!(p_line.contains("charlie"));
    }

    #[test]
    fn end_to_end_no_broadcast_skips_append() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = write_config(tmp.path(), config_with_broadcast_text());
        let args = Args {
            config: cfg_path.clone(),
            name: "charlie".into(),
            workdir: "/home/me/charlie".into(),
            role: "Testing.".into(),
            platform: "wsl".into(),
            peers: vec!["alice".into()],
            bench_scheduler: false,
            no_broadcast: true,
            template: None,
            dry_run: false,
            code_root: None,
            host: None,
        };
        run(args).unwrap();
        let updated = fs::read_to_string(&cfg_path).unwrap();
        let bsec = updated.split(r#"file = "_broadcast.md""#).nth(1).unwrap();
        let p_line = bsec.lines().find(|l| l.contains("participants =")).unwrap();
        assert!(!p_line.contains("charlie"));
    }

    #[test]
    fn end_to_end_refuses_to_overwrite_existing_template() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = write_config(tmp.path(), minimal_config_text());
        fs::create_dir_all(tmp.path().join("agents")).unwrap();
        fs::write(tmp.path().join("agents/charlie.md"), "pre-existing").unwrap();

        let args = base_args(cfg_path.clone());
        let err = run(args).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        // Config should also remain unchanged on this failure path —
        // well, actually we DO write the config first and then catch
        // the template error. Worth knowing the failure semantics.
        // Document that here:
        let pre = fs::read_to_string(tmp.path().join("agents/charlie.md")).unwrap();
        assert_eq!(pre, "pre-existing", "we did not clobber existing template");
    }

    #[test]
    fn end_to_end_validates_after_edit() {
        // Use a config that would become invalid if we added a bad channel.
        // Easiest: a config with windows_inbox missing, then try to add a
        // peer where derived channel would need side=windows. Preflight
        // catches the bad peer ordering before we get here, so the
        // simpler check is just that the happy path validates.
        let tmp = TempDir::new().unwrap();
        let cfg_path = write_config(tmp.path(), minimal_config_text());
        let args = base_args(cfg_path.clone());
        run(args).unwrap();
        // Re-validate would error if the edit broke anything.
        let cfg = Config::load(&cfg_path).unwrap();
        cfg.validate().unwrap();
    }

    // ----- template rendering -----------------------------------------

    #[test]
    fn template_includes_slug_and_role() {
        let args = base_args(PathBuf::new());
        let body = render_template(&args);
        assert!(body.starts_with("# charlie agent"));
        assert!(body.contains("**Testing.**"));
        assert!(body.contains("giga watch --as charlie"));
    }

    #[test]
    fn template_lists_peers() {
        let mut args = base_args(PathBuf::new());
        args.peers = vec!["alice".into(), "bob".into()];
        let body = render_template(&args);
        assert!(body.contains("`alice`"));
        assert!(body.contains("`bob`"));
    }

    #[test]
    fn template_handles_no_peers() {
        let mut args = base_args(PathBuf::new());
        args.peers = vec![];
        let body = render_template(&args);
        assert!(body.contains("no bilateral peers yet"));
    }
}
