//! `giga post` — append a properly-formatted message to an inbox channel.
//!
//! Enforces the convention so agents can't accidentally drop the
//! header block or forget the WAITING ON / informational tag.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::config::Config;
use crate::foundation::append::append_with_lock;

pub struct Args {
    pub channel: String,
    pub me: String,
    pub subject: String,
    pub body: Option<String>,
    pub waiting_on: Option<String>,
    pub needs: Option<String>,
    pub config: PathBuf,
    /// v0.4.0 (BROADCAST_FANOUT_DESIGN.md): list of agent slugs to
    /// address this broadcast to. Synthesizes a `[ack: a, b, c]`
    /// subject prefix; recipients-only fire on the receiving end's
    /// watchers, other participants on the channel stay silent. No-op
    /// on non-broadcast channels (the prefix is only interpreted on
    /// `_*.md` files).
    pub to: Vec<String>,
    /// v0.4.0: mark this post informational — synthesizes a `[fyi]`
    /// subject prefix. Watchers archive it to per-agent log instead
    /// of firing a Monitor notification (zero LLM cost). No-op on
    /// non-broadcast channels. Mutually exclusive with `--to`.
    pub fyi: bool,
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
    // local single-writer slice <channel>.<this_host>.md.
    //
    // v0.3.5 (REMOTE_DUAL_WRITE_DESIGN.md): for cross-host channels,
    // ALSO dual-write the same frame directly to the merged
    // <channel>.md so local watchers see the post without depending
    // on the merger daemon's liveness. Pre-v0.3.5 the merger was
    // load-bearing for local visibility — adding one remote agent to
    // a channel silently disrupted neo↔neo posts on that channel
    // whenever the merger was lagging, crashed, or hadn't been
    // spawned (v0.3.4 F11). Dual-write removes that coupling.
    //
    // Ordering: slice FIRST, then merged. If slice succeeds and
    // merged fails, peers still receive the message via sync; local
    // visibility recovers once the merger reads own slice as a
    // fallback (own-slice cursor initialized to EOF on merger start,
    // so only POST-failure bytes get re-appended). If merged
    // succeeded first and slice failed, local would see a post that
    // peers never receive — silent divergence. Avoid that.
    //
    // For all-local channels (or pre-remote-channels configs with no
    // [[hosts]]), keep today's fast-path direct write to <channel>.md
    // (the slice IS the merged file in that case).
    let (primary_path, secondary_path) = match (cfg_opt.as_ref(), channel_entry) {
        (Some(cfg), Some(ch)) if !cfg.channel_is_local(ch) => {
            let this_host = cfg.this_host.as_deref().ok_or_else(|| {
                anyhow!(
                    "channel `{}` spans hosts but this_host is unknown — \
                     create a sibling this_host.toml with `this_host = \"<host>\"`",
                    ch.file,
                )
            })?;
            (
                slice_path(&merged_path, this_host),
                Some(merged_path.clone()),
            )
        }
        _ => (merged_path.clone(), None),
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

    // v0.4.0: synthesize broadcast prefix into the subject when --to
    // or --fyi was passed. The prefix is what `giga watch` parses to
    // decide whether to filter or stagger this message. No-op on
    // non-broadcast channels (the prefix is harmless extra text in
    // the subject; watchers only honor it for `_*.md` files).
    if args.fyi && !args.to.is_empty() {
        return Err(anyhow!("--fyi and --to are mutually exclusive"));
    }
    let subject_with_prefix = if args.fyi {
        format!("[fyi] {}", args.subject)
    } else if !args.to.is_empty() {
        format!("[ack: {}] {}", args.to.join(", "), args.subject)
    } else {
        args.subject.clone()
    };

    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let block = format_block(
        &args.me,
        &subject_with_prefix,
        &ts,
        &body,
        args.waiting_on.as_deref(),
        args.needs.as_deref(),
    );

    // 1) Primary write — slice (cross-host) or merged file (local). Must
    //    succeed; failure here errors the call.
    append_with_lock(&primary_path, block.as_bytes())
        .with_context(|| format!("writing primary {}", primary_path.display()))?;

    // 2) Optional secondary write — merged file when primary was a slice.
    //    Surface partial failure but don't fail the call: slice already
    //    has the frame so peers will eventually see it via sync, and the
    //    merger's own-slice fallback will catch up local visibility.
    if let Some(secondary) = &secondary_path {
        if let Err(e) = append_with_lock(secondary, block.as_bytes()) {
            eprintln!(
                "post: warning — slice {} ok but merged {} failed: {e}",
                primary_path.display(),
                secondary.display(),
            );
        }
    }

    println!(
        "posted to {} ({} bytes)",
        primary_path.display(),
        block.len()
    );
    Ok(())
}

// The locked append (and its plain-append fallback) now live in
// `foundation::append` — shared by post, merger, and the watcher FYI
// archive. `append_with_lock` is imported at the top of this module.

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

    // append_with_lock behavior (locked happy path, lock release, missing-
    // file creation, torn-write resistance) is tested in foundation::append.

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
            .map(|n| {
                format!("[[hosts]]\nname = \"{n}\"\ntailnet_hostname = \"{n}.tail0.ts.net\"\n")
            })
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

    /// v0.3.5 T1 (REMOTE_DUAL_WRITE_DESIGN.md): on a cross-host channel
    /// the post writes to BOTH the per-host slice (for sync to ship to
    /// peers) AND the merged file (so local watchers see it without
    /// depending on the merger daemon).
    #[test]
    fn run_dual_writes_to_slice_and_merged_for_cross_host_channel() {
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
            to: vec![],
            fyi: false,
        })
        .unwrap();

        let slice = inbox.join("alice-bob.wsl-a.md");
        let merged = inbox.join("alice-bob.md");
        assert!(slice.exists(), "slice file should be created (for sync)");
        assert!(
            merged.exists(),
            "merged file should be created (for local watcher)"
        );

        // Frame must be identical in both files: it's the same write_all
        // bytes constructed once.
        let slice_body = fs::read_to_string(&slice).unwrap();
        let merged_body = fs::read_to_string(&merged).unwrap();
        assert!(slice_body.contains("[alice] ping"));
        assert!(slice_body.contains("hello"));
        assert_eq!(
            slice_body, merged_body,
            "dual-write must produce byte-identical content in both files"
        );
    }

    /// v0.3.5 T5 (the headline use case from REMOTE_DUAL_WRITE_DESIGN.md):
    /// after a cross-host post, the merged file is immediately readable
    /// without any merger tick having run. This is the assertion that
    /// "adding one remote agent must not disrupt local comms" holds.
    #[test]
    fn cross_host_post_is_visible_in_merged_without_merger_tick() {
        let (tmp, config_path) = swarm_fixture(&["wsl-a", "wsl-b"], "wsl-a");
        let inbox = tmp.path().join("inbox");

        run(Args {
            channel: "alice-bob.md".into(),
            me: "alice".into(),
            subject: "design-question".into(),
            body: Some("immediately visible to local watcher".into()),
            waiting_on: None,
            needs: None,
            config: config_path,
            to: vec![],
            fyi: false,
        })
        .unwrap();

        // The local watcher tails the merged file. With dual-write, the
        // post-tail observation is immediate — no merger required.
        let merged = inbox.join("alice-bob.md");
        let body = fs::read_to_string(&merged).unwrap();
        assert!(body.contains("[alice] design-question"));
        assert!(body.contains("immediately visible to local watcher"));
    }

    /// v0.3.5 T2+T3 (REMOTE_DUAL_WRITE_DESIGN.md): when merged write
    /// fails (e.g., merged path is a non-writable directory), the
    /// slice write must still have landed first AND `run` must return
    /// Ok (slice is the canonical record; peers will get the frame via
    /// sync). The merger's own-slice fallback will catch up local
    /// visibility on the next tick.
    ///
    /// Repro: replace the merged file path with a directory after
    /// init so the OS rejects the OpenOptions::open call for the
    /// merged write. The slice path is unaffected.
    #[test]
    fn run_returns_ok_and_keeps_slice_when_merged_write_fails() {
        let (tmp, config_path) = swarm_fixture(&["wsl-a", "wsl-b"], "wsl-a");
        let inbox = tmp.path().join("inbox");
        // Make the merged path a directory — any append-open against
        // it will EISDIR and the merged write will fail.
        let merged_path = inbox.join("alice-bob.md");
        fs::create_dir_all(&merged_path).unwrap();

        let result = run(Args {
            channel: "alice-bob.md".into(),
            me: "alice".into(),
            subject: "ping".into(),
            body: Some("hello".into()),
            waiting_on: None,
            needs: None,
            config: config_path,
            to: vec![],
            fyi: false,
        });

        assert!(
            result.is_ok(),
            "post must return Ok when slice ok and merged fails: got {result:?}"
        );

        // Slice still has the frame (slice-first ordering held).
        let slice = inbox.join("alice-bob.wsl-a.md");
        let slice_body = fs::read_to_string(&slice).unwrap();
        assert!(slice_body.contains("[alice] ping"));
        assert!(slice_body.contains("hello"));
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
            to: vec![],
            fyi: false,
        })
        .unwrap();

        let merged = inbox.join("alice-bob.md");
        assert!(
            merged.exists(),
            "local channel writes directly to merged path"
        );
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
            to: vec![],
            fyi: false,
        })
        .unwrap();

        assert!(inbox.join("alice-bob.md").exists());
    }

    /// v0.4.0 (BROADCAST_FANOUT_DESIGN.md): `--to alice,bob` synthesizes
    /// `[ack: alice, bob]` into the subject. The header parser on the
    /// watcher side then filters notifications to the named agents.
    #[test]
    fn post_with_to_flag_synthesizes_ack_prefix_in_subject() {
        let tmp = tempfile::TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let cfg_text = format!(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = '{}'
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
[[agents]]
name = "carol"
workdir = "/h/carol"
role = "."
platform = "wsl"
[[channels]]
file = "_broadcast.md"
side = "wsl"
participants = ["alice", "bob", "carol"]
"#,
            inbox.display(),
        );
        let config_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&config_path, cfg_text).unwrap();

        run(Args {
            channel: "_broadcast.md".into(),
            me: "alice".into(),
            subject: "cleanup nudge".into(),
            body: Some("ping".into()),
            waiting_on: None,
            needs: None,
            config: config_path,
            to: vec!["bob".into(), "carol".into()],
            fyi: false,
        })
        .unwrap();

        let body = std::fs::read_to_string(inbox.join("_broadcast.md")).unwrap();
        assert!(
            body.contains("[ack: bob, carol] cleanup nudge"),
            "subject must carry the ack-prefix:\n{body}"
        );
    }

    /// v0.4.0: `--fyi` synthesizes the `[fyi]` prefix; watchers
    /// archive instead of firing notifications.
    #[test]
    fn post_with_fyi_flag_synthesizes_fyi_prefix_in_subject() {
        let tmp = tempfile::TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let cfg_text = format!(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = '{}'
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
file = "_broadcast.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox.display(),
        );
        let config_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&config_path, cfg_text).unwrap();

        run(Args {
            channel: "_broadcast.md".into(),
            me: "alice".into(),
            subject: "host-c online".into(),
            body: Some("FYI".into()),
            waiting_on: None,
            needs: None,
            config: config_path,
            to: vec![],
            fyi: true,
        })
        .unwrap();

        let body = std::fs::read_to_string(inbox.join("_broadcast.md")).unwrap();
        assert!(
            body.contains("[fyi] host-c online"),
            "subject must carry the fyi-prefix:\n{body}"
        );
    }

    /// v0.4.0: --fyi + --to is rejected at the post handler.
    #[test]
    fn post_rejects_fyi_with_to_combination() {
        let tmp = tempfile::TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let cfg_text = format!(
            r#"
[project]
name = "t"
[paths]
wsl_inbox = '{}'
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
file = "_broadcast.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox.display(),
        );
        let config_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&config_path, cfg_text).unwrap();

        let err = run(Args {
            channel: "_broadcast.md".into(),
            me: "alice".into(),
            subject: "x".into(),
            body: Some("x".into()),
            waiting_on: None,
            needs: None,
            config: config_path,
            to: vec!["bob".into()],
            fyi: true,
        })
        .unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }
}
