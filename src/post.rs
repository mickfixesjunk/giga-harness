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

    let path = resolve(&args.channel, cfg_opt.as_ref(), &args.config)?;

    // Validate sender is a participant on this channel (when we have a config).
    if let Some(cfg) = &cfg_opt {
        if let Some(ch) = cfg.channels.iter().find(|c| c.file == args.channel || cfg.channel_path(c).map(|p| p == path).unwrap_or(false)) {
            if !ch.participants.contains(&args.me) {
                return Err(anyhow!(
                    "`{}` is not a participant of channel `{}` (participants: {:?})",
                    args.me, ch.file, ch.participants
                ));
            }
            if let Some(target) = &args.waiting_on {
                if !ch.participants.contains(target) {
                    return Err(anyhow!(
                        "WAITING ON target `{}` is not a participant of channel `{}`",
                        target, ch.file
                    ));
                }
            }
        }
    }

    let body = match args.body {
        Some(b) => b,
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).context("reading body from stdin")?;
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
        .open(&path)
        .with_context(|| format!("opening {} for append", path.display()))?;
    f.write_all(block.as_bytes())
        .with_context(|| format!("writing to {}", path.display()))?;

    println!("posted to {} ({} bytes)", path.display(), block.len());
    Ok(())
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
        if let Some(ch) = cfg.channels.iter().find(|c| c.file == channel) {
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
        let out = format_block("design", "ping", TS, "body", Some("web"), Some("answer to Q1"));
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
}
