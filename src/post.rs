//! `giga post` — append a properly-formatted message to an inbox channel.
//!
//! Enforces the convention so agents can't accidentally drop the
//! header block or forget the WAITING ON / informational tag.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::config::Config;

pub struct Args {
    pub channel: String,
    pub me: String,
    pub subject: String,
    pub body: Option<String>,
    pub waiting_on: Option<String>,
    pub needs: Option<String>,
    pub config: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    let cfg_opt = Config::load(&args.config).ok();

    let merged_path = resolve(&args.channel, cfg_opt.as_ref(), &args.config)?;

    // Find the channel entry once; reused for participant validation and
    // the local-vs-remote routing decision.
    let channel_entry = cfg_opt.as_ref().and_then(|cfg| {
        cfg.channels.iter().find(|c| {
            c.file == args.channel
                || cfg
                    .channel_path(c)
                    .map(|p| p == merged_path)
                    .unwrap_or(false)
        })
    });

    if let (Some(cfg), Some(ch)) = (cfg_opt.as_ref(), channel_entry) {
        if !ch.participants.contains(&args.me) {
            return Err(anyhow!(
                "`{}` is not a participant of channel `{}` (participants: {:?})",
                args.me,
                ch.file,
                ch.participants
            ));
        }
        if let Some(target) = &args.waiting_on {
            if !ch.participants.contains(target) {
                return Err(anyhow!(
                    "WAITING ON target `{}` is not a participant of channel `{}`",
                    target,
                    ch.file
                ));
            }
        }
        let _ = cfg; // silence unused warning when no slice routing needed
    }

    // Cross-host routing: when the channel spans hosts, append to the
    // local single-writer slice <channel>.<this_host>.md instead of the
    // merged <channel>.md. The local merger (step 4) appends slice
    // events into the merged file for the watcher to read.
    //
    // For all-local channels (or pre-remote-channels configs with no
    // [[hosts]]), keep today's fast-path direct write to <channel>.md.
    let write_path = match (cfg_opt.as_ref(), channel_entry) {
        (Some(cfg), Some(ch)) if !cfg.channel_is_local(ch) => {
            let this_host = cfg.this_host.as_deref().ok_or_else(|| {
                anyhow!(
                    "channel `{}` spans hosts but this_host is unknown — \
                     create a sibling this_host.toml with `this_host = \"<host>\"`",
                    ch.file,
                )
            })?;
            slice_path(&merged_path, this_host)
        }
        _ => merged_path.clone(),
    };

    let body = match args.body {
        Some(b) => b,
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("reading body from stdin")?;
            s
        }
    };

    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let block = format_block(
        &args.me,
        &args.subject,
        &ts,
        &body,
        args.waiting_on.as_deref(),
        args.needs.as_deref(),
    );

    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&write_path)
        .with_context(|| format!("opening {} for append", write_path.display()))?;
    f.write_all(block.as_bytes())
        .with_context(|| format!("writing to {}", write_path.display()))?;

    println!("posted to {} ({} bytes)", write_path.display(), block.len());
    Ok(())
}

/// Derive the per-host slice file path from the merged channel path.
///
/// Given `/dir/<channel>.md` + this_host = `wsl-a`, returns
/// `/dir/<channel>.wsl-a.md`. Pure — testable without filesystem.
///
/// The slice file is the single-writer wire format that `sync` mirrors
/// between hosts. The merger reads from all slice files and appends to
/// the merged `<channel>.md` that the watcher tails.
fn slice_path(merged: &Path, this_host: &str) -> PathBuf {
    let parent = merged.parent().unwrap_or_else(|| Path::new("."));
    let stem = merged
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "channel".to_string());
    parent.join(format!("{stem}.{this_host}.md"))
}

/// Pure message-block formatter — extracted so we can unit-test the
/// header/footer rules without touching the filesystem or clock. The
/// timestamp is passed in (caller produces it from `chrono::Utc::now()`
/// in real use; tests pass a fixed value).
fn format_block(
    sender: &str,
    subject: &str,
    ts: &str,
    body: &str,
    waiting_on: Option<&str>,
    needs: Option<&str>,
) -> String {
    let footer = match (waiting_on, needs) {
        (Some(who), Some(needs)) => format!("WAITING ON: {who} ({needs})"),
        (Some(who), None) => format!("WAITING ON: {who}"),
        (None, _) => "(Informational, no response required.)".to_string(),
    };
    format!(
        "\n\n===\n[{sender}] {subject} — {ts}\n===\n\n{}\n\n{footer}\n===\n",
        body.trim_end(),
    )
}

fn resolve(channel: &str, cfg: Option<&Config>, config_path: &Path) -> Result<PathBuf> {
    let as_path = Path::new(channel);
    if as_path.is_absolute() && as_path.parent().map(|p| p.exists()).unwrap_or(false) {
        return Ok(as_path.to_path_buf());
    }
    if let Some(cfg) = cfg {
        // Channel files in config always carry the `.md` suffix. Accept
        // bare names from the caller so agents don't have to remember it
        // (`giga post pipeline-usage` ≡ `giga post pipeline-usage.md`).
        let with_md = if channel.ends_with(".md") {
            None
        } else {
            Some(format!("{channel}.md"))
        };
        if let Some(ch) = cfg
            .channels
            .iter()
            .find(|c| c.file == channel || with_md.as_deref().map(|m| c.file == m).unwrap_or(false))
        {
            return cfg.channel_path(ch);
        }
    }
    if as_path.parent().map(|p| p.exists()).unwrap_or(false) {
        return Ok(as_path.to_path_buf());
    }
    if !config_path.exists() {
        return Err(anyhow!(
            "no config file at {} — pass --config <path>, or place a giga-harness.toml in this directory (a workdir symlink to the project config is the usual fix)",
            config_path.display(),
        ));
    }
    Err(anyhow!(
        "channel `{channel}` not listed in {} and not a valid path",
        config_path.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: &str = "2026-05-25T12:00:00Z";

    #[test]
    fn informational_block_uses_no_response_required_footer() {
        let out = format_block("design", "online", TS, "hi", None, None);
        assert!(out.contains("[design] online — 2026-05-25T12:00:00Z"));
        assert!(out.contains("(Informational, no response required.)"));
        assert!(!out.contains("WAITING ON"));
    }

    #[test]
    fn waiting_on_without_needs() {
        let out = format_block("design", "ping", TS, "body", Some("web"), None);
        assert!(out.contains("WAITING ON: web"));
        assert!(!out.contains("("));
    }

    #[test]
    fn waiting_on_with_needs() {
        let out = format_block(
            "design",
            "ping",
            TS,
            "body",
            Some("web"),
            Some("answer to Q1"),
        );
        assert!(out.contains("WAITING ON: web (answer to Q1)"));
    }

    #[test]
    fn needs_without_waiting_on_is_ignored() {
        // (None, Some(needs)) hits the (None, _) arm — informational.
        let out = format_block("design", "ping", TS, "body", None, Some("ignored"));
        assert!(out.contains("(Informational, no response required.)"));
        assert!(!out.contains("ignored"));
    }

    #[test]
    fn block_trims_trailing_body_whitespace() {
        let out = format_block("design", "s", TS, "body line\n\n\n", None, None);
        // The body line should be followed by exactly two blank lines
        // before the footer (the literal `\n\n` we emit after the body).
        assert!(out.contains("body line\n\n(Informational"));
        // No extra trailing blanks from the body itself:
        assert!(!out.contains("body line\n\n\n\n"));
    }

    #[test]
    fn block_has_canonical_header_footer_structure() {
        let out = format_block("a", "subject here", TS, "body", None, None);
        // Two leading newlines (separator from prior message), then ===
        assert!(out.starts_with("\n\n===\n"));
        assert!(out.ends_with("\n===\n"));
        // Three === lines total: header opener, header closer, footer closer.
        assert_eq!(out.matches("===").count(), 3);
    }

    #[test]
    fn empty_body_still_produces_valid_block() {
        let out = format_block("a", "s", TS, "", None, None);
        assert!(out.contains("[a] s — 2026-05-25T12:00:00Z"));
        assert!(out.contains("(Informational, no response required.)"));
    }

    // -------------------------------------------------------------------
    // Cross-host slice path tests (per REMOTE_DESIGN.md step 3).
    // slice_path() is pure; the routing decision in run() is exercised
    // by integration tests that write real files.
    // -------------------------------------------------------------------

    #[test]
    fn slice_path_inserts_host_before_md_extension() {
        let merged = std::path::Path::new("/inbox/design-code-2.md");
        let slice = slice_path(merged, "wsl-a");
        assert_eq!(
            slice,
            std::path::PathBuf::from("/inbox/design-code-2.wsl-a.md"),
        );
    }

    #[test]
    fn slice_path_handles_channel_name_with_dots() {
        // A channel like `foo.bar.md` should slice to `foo.bar.<host>.md`,
        // not `foo.<host>.md` — file_stem only strips the final extension.
        let merged = std::path::Path::new("/inbox/foo.bar.md");
        let slice = slice_path(merged, "h1");
        assert_eq!(slice, std::path::PathBuf::from("/inbox/foo.bar.h1.md"));
    }

    #[test]
    fn slice_path_preserves_inbox_dir() {
        let merged = std::path::Path::new("/some/deep/path/to/inbox/ch.md");
        let slice = slice_path(merged, "h2");
        assert_eq!(
            slice,
            std::path::PathBuf::from("/some/deep/path/to/inbox/ch.h2.md"),
        );
    }

    #[test]
    fn slice_path_with_relative_path() {
        // Edge case — channel paths are always absolute via Config::channel_path
        // in practice, but the helper should still produce a sensible name.
        let merged = std::path::Path::new("ch.md");
        let slice = slice_path(merged, "wsl-b");
        assert_eq!(slice, std::path::PathBuf::from("ch.wsl-b.md"));
    }

    // -------------------------------------------------------------------
    // run() integration tests — exercise the local-vs-slice routing
    // decision against real config files + temp inbox dirs.
    // -------------------------------------------------------------------

    use std::fs;
    use tempfile::TempDir;

    /// Build a swarm fixture with the given hosts + 2 agents (one per host)
    /// + a bilateral channel. Returns (tmpdir, config_path).
    fn swarm_fixture(host_names: &[&str], this_host: &str) -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let config_path = tmp.path().join("giga-harness.toml");

        let hosts_toml: String = host_names
            .iter()
            .map(|n| format!("[[hosts]]\nname = \"{n}\"\ntailnet_hostname = \"{n}.tail0.ts.net\"\n"))
            .collect();
        let toml = format!(
            r#"
[project]
name = "remote-test"

[paths]
wsl_inbox = '{inbox}'

{hosts_toml}

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "{a}"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"
host = "{b}"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox = inbox.to_string_lossy(),
            a = host_names[0],
            b = host_names.get(1).copied().unwrap_or(host_names[0]),
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
    fn run_writes_to_slice_file_when_channel_spans_hosts() {
        let (tmp, config_path) = swarm_fixture(&["wsl-a", "wsl-b"], "wsl-a");
        let inbox = tmp.path().join("inbox");

        run(Args {
            channel: "alice-bob.md".into(),
            me: "alice".into(),
            subject: "ping".into(),
            body: Some("hello".into()),
            waiting_on: None,
            needs: None,
            config: config_path,
        })
        .unwrap();

        // The slice file should exist with our message; the merged file
        // should NOT (merger is the only writer to <channel>.md).
        let slice = inbox.join("alice-bob.wsl-a.md");
        let merged = inbox.join("alice-bob.md");
        assert!(slice.exists(), "slice file should be created");
        assert!(!merged.exists(), "merged file shouldn't exist yet — merger writes it");

        let body = fs::read_to_string(&slice).unwrap();
        assert!(body.contains("[alice] ping"));
        assert!(body.contains("hello"));
    }

    #[test]
    fn run_writes_to_merged_file_when_channel_is_local_only() {
        // Same fixture but with only one host — channel is fully local,
        // fast-path direct write to <channel>.md (today's behavior).
        let (tmp, config_path) = swarm_fixture(&["wsl-only"], "wsl-only");
        let inbox = tmp.path().join("inbox");

        // Both alice + bob get host=wsl-only -> participants share host
        // -> channel_is_local -> direct write.
        run(Args {
            channel: "alice-bob.md".into(),
            me: "alice".into(),
            subject: "hi".into(),
            body: Some("local".into()),
            waiting_on: None,
            needs: None,
            config: config_path,
        })
        .unwrap();

        let merged = inbox.join("alice-bob.md");
        assert!(merged.exists(), "local channel writes directly to merged path");
        let body = fs::read_to_string(&merged).unwrap();
        assert!(body.contains("[alice] hi"));
        // And no slice file was created (we're in fast-path mode):
        for name in ["alice-bob.wsl-only.md", "alice-bob.wsl-a.md"] {
            assert!(
                !inbox.join(name).exists(),
                "slice file {name} shouldn't exist in fast-path mode",
            );
        }
    }

    #[test]
    fn run_writes_to_merged_file_for_legacy_local_only_swarm() {
        // Pre-remote-channels config: no [[hosts]] at all. Should behave
        // exactly as today — write straight to the merged file.
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let config_path = tmp.path().join("giga-harness.toml");
        let toml = format!(
            r#"
[project]
name = "legacy"

[paths]
wsl_inbox = '{inbox}'

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox = inbox.to_string_lossy(),
        );
        fs::write(&config_path, toml).unwrap();

        run(Args {
            channel: "alice-bob.md".into(),
            me: "alice".into(),
            subject: "legacy".into(),
            body: Some("ok".into()),
            waiting_on: None,
            needs: None,
            config: config_path,
        })
        .unwrap();

        assert!(inbox.join("alice-bob.md").exists());
    }
}
