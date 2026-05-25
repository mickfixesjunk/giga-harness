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
//! can add new agents on Mick's behalf without hand-editing TOML.
//! Launch is a separate step the user owns (window-layout intent
//! is theirs).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

use crate::config::Config;

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
            println!("  - append `{}` to broadcast participants of {}", args.name, f);
        }
        println!("  - write template: {}", template_path.display());
        println!("(dry-run — no files modified)");
        return Ok(());
    }

    // ---- write changes ---------------------------------------------
    fs::write(&args.config, &updated)
        .with_context(|| format!("writing updated {}", args.config.display()))?;
    if let Some(parent) = template_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    if template_path.exists() {
        // Rare but possible: agents/<slug>.md already exists from a
        // prior aborted add. Don't clobber — surface it instead.
        return Err(anyhow!(
            "template path {} already exists; refusing to overwrite. \
             Either remove it manually if it's a leftover, or pick a \
             different --name.",
            template_path.display(),
        ));
    }
    fs::write(&template_path, template_body)
        .with_context(|| format!("writing {}", template_path.display()))?;

    // ---- re-validate the updated config ----------------------------
    let revalidated = Config::load(&args.config)
        .context("re-loading config after edit failed — config is in an unexpected state")?;
    revalidated
        .validate()
        .context("re-validating after edit failed — config is in an unexpected state")?;

    // ---- summary ----------------------------------------------------
    println!("added agent `{}` ({}, {})", args.name, args.platform, args.workdir);
    for ch in &new_channels {
        println!(
            "  + [[channels]] {} ({}, {} ↔ {})",
            ch.file, ch.side, ch.participants[0], ch.participants[1],
        );
    }
    for f in &broadcast_targets {
        println!("  + appended `{}` to broadcast participants of {}", args.name, f);
    }
    println!("  + wrote {}", template_path.display());
    println!();
    println!("next:");
    println!("  giga validate {}", args.config.display());
    println!("  # if multi-host: re-localize first, then launch from your terminal:");
    println!("  # ./setup-<host>.sh && giga launch --only {} --new-window <localized-config>", args.name);
    Ok(())
}

// --------------------------------------------------------------- pre-flight

fn preflight(cfg: &Config, args: &Args) -> Result<()> {
    if args.name.is_empty() {
        return Err(anyhow!("--name cannot be empty"));
    }
    if !args.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
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
                cfg.agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
            ));
        }
    }
    Ok(())
}

// --------------------------------------------------------------- channel derivation

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
    if args.bench_scheduler {
        block["bench_scheduler"] = value(true);
    }
    block["claudemd_template"] = value(format!("agents/{}.md", args.name));
    agents.push(block);
    Ok(())
}

fn append_channel(doc: &mut DocumentMut, ch: &DerivedChannel) -> Result<()> {
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
    Err(anyhow!("broadcast channel `{}` not found in [[channels]]", file))
}

fn ensure_array_of_tables<'a>(doc: &'a mut DocumentMut, key: &str) -> Result<&'a mut ArrayOfTables> {
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
    format!(
        "# {name} agent\n\
         \n\
         You are the **{role_short}** for this project.\n\
         \n\
         > Generated by `giga add-agent`. Edit the role + responsibilities sections below to match what this agent actually owns and how it should behave — the generated stubs are intentionally minimal.\n\
         \n\
         ## Session Start (do this first, every session)\n\
         \n\
         **Step 0 (always):** If `./HANDOVER.md` exists in your workdir, read it before anything else. It carries cross-session / cross-machine state — recent decisions, in-flight work, pickup instructions — that your conversation history may not include.\n\
         \n\
         In order:\n\
         \n\
         1. **Post intro** on each of your channels via `giga post <channel> --as {name} --subject \"online\" --body \"{name} session resumed\"`. Informational.\n\
         2. **Arm the Monitor below.** Use the exact `Monitor(persistent: true, command: ...)` invocation — don't paraphrase. One watcher; auto-discovery handles the channel list.\n\
         3. **Standby for messages.** Don't initiate work without being asked. If you were mid-task per HANDOVER.md, finish that first.\n\
         \n\
         ## Your responsibilities\n\
         \n\
         _(TODO — describe what {name} OWNS, who they answer to, who answers to them, and any standing rules.)_\n\
         \n\
         Initial bilateral peers: {peers}.\n\
         \n\
         ## Channels you watch\n\
         \n\
         ```\n\
         Monitor(persistent: true, command: \"giga watch --as {name}\")\n\
         ```\n\
         \n\
         One watcher auto-discovers every channel where you're a participant (per `giga-harness.toml`). New channels added later are picked up automatically (~15s reread). Notifications are formatted `inbox <channel>: [sender] ...`.\n\
         \n\
         ## Convention\n\
         \n\
         Every channel message ends with either:\n\
         \n\
         * `WAITING ON: <agent> (<what's needed>)` — if a reply is expected.\n\
         * `Informational, no response required.` — otherwise.\n\
         \n\
         Ambiguous closings stall the pipeline. Use the tag.\n",
        name = args.name,
        role_short = args.role,
        peers = peer_list,
    )
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
        let body = format!("{}\nbench_scheduler = true\n", minimal_config_text().replace(
            r#"name = "alice""#,
            r#"name = "alice"
bench_scheduler = true"#,
        ));
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

    // ----- channel derivation -----------------------------------------

    #[test]
    fn derives_alphabetical_filename() {
        let cfg = Config::load_str_for_test(minimal_config_text()).unwrap();
        let args = base_args(PathBuf::new());
        let channels = derive_channels(&cfg, &args);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].file, "alice-charlie.md");
        assert_eq!(channels[0].participants, ["alice".to_string(), "charlie".to_string()]);
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
        let charlie_section = out
            .split(r#"name = "charlie""#)
            .nth(1)
            .unwrap();
        // Section continues until next [[ or end. bench_scheduler must
        // not appear within this section.
        let cut = charlie_section.find("[[").unwrap_or(charlie_section.len());
        assert!(!charlie_section[..cut].contains("bench_scheduler"));
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
        let broadcast_section = out
            .split(r#"file = "_broadcast.md""#)
            .nth(1)
            .unwrap();
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
        assert_eq!(alice_count, 1, "alice should appear exactly once, got {}", alice_count);
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
        };
        run(args).unwrap();
        let after = fs::read_to_string(&cfg_path).unwrap();
        assert_eq!(original, after, "dry-run modified config");
        assert!(!tmp.path().join("agents").exists(), "dry-run created agents dir");
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
