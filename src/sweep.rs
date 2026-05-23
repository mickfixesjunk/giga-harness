//! `giga sweep` — tabulate every channel's last message + WAITING ON tag.
//!
//! Replaces the project-specific `channel-state.sh` script. Run from
//! the coordinator's terminal every few minutes to spot stalled channels.

use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::config::Config;

pub fn run(config_path: &Path, owed_by_filter: Option<&str>) -> Result<()> {
    let cfg = Config::load(config_path)?;

    let mut rows: Vec<Row> = Vec::new();
    for ch in &cfg.channels {
        let path = cfg.channel_path(ch)?;
        if !path.exists() {
            rows.push(Row {
                channel: ch.file.clone(),
                last_from: "(no file)".into(),
                subject: "—".into(),
                waiting_on: None,
            });
            continue;
        }
        let body = fs::read_to_string(&path).unwrap_or_default();
        let last = last_header_block(&body);
        rows.push(last.unwrap_or_else(|| Row {
            channel: ch.file.clone(),
            last_from: "(empty)".into(),
            subject: "—".into(),
            waiting_on: None,
        }).with_channel(ch.file.clone()));
    }

    let filtered: Vec<&Row> = if let Some(who) = owed_by_filter {
        rows.iter().filter(|r| r.waiting_on.as_deref() == Some(who)).collect()
    } else {
        rows.iter().collect()
    };

    println!("{:<35} {:<15} {:<50} {}", "channel", "last_from", "subject", "waiting_on");
    println!("{}", "-".repeat(120));
    for r in &filtered {
        let wait = r.waiting_on.clone().unwrap_or_else(|| "informational".into());
        let subj = trunc(&r.subject, 48);
        println!("{:<35} {:<15} {:<50} {}", r.channel, r.last_from, subj, wait);
    }

    if owed_by_filter.is_none() {
        let pending: Vec<&&Row> = filtered.iter().filter(|r| r.waiting_on.is_some()).collect();
        println!("\n{} channels with open WAITING ON tag", pending.len());
    }
    Ok(())
}

struct Row {
    channel: String,
    last_from: String,
    subject: String,
    waiting_on: Option<String>,
}

impl Row {
    fn with_channel(mut self, c: String) -> Self {
        self.channel = c;
        self
    }
}

/// Walk the file backwards to find the most recent header line:
///   `[sender] subject — timestamp`
/// Then scan forward in the same block for a WAITING ON line.
fn last_header_block(body: &str) -> Option<Row> {
    let lines: Vec<&str> = body.lines().collect();
    let mut last_header_idx: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(stripped) = line.strip_prefix('[') {
            if let Some(end) = stripped.find("] ") {
                // Sanity: looks like `[name] subject` — accept.
                if end > 0 && stripped.len() > end + 2 {
                    last_header_idx = Some(i);
                }
            }
        }
    }
    let idx = last_header_idx?;
    let header = lines[idx];
    let inner = &header[1..];
    let bracket_end = inner.find("] ")?;
    let sender = &inner[..bracket_end];
    let rest = &inner[bracket_end + 2..];
    let subject = rest
        .rsplit_once('—')
        .map(|(s, _)| s.trim().to_string())
        .unwrap_or_else(|| rest.to_string());

    let mut waiting_on: Option<String> = None;
    for line in lines.iter().skip(idx + 1) {
        if let Some(rest) = line.strip_prefix("WAITING ON: ") {
            let who = rest.split_whitespace().next().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
            // Treat synonyms-for-informational as not-waiting.
            let lower = who.to_ascii_lowercase();
            let synonymous = matches!(lower.as_str(), "nobody" | "none" | "no-one" | "noone" | "n/a" | "informational");
            if !who.is_empty() && !synonymous {
                waiting_on = Some(who.to_string());
            }
            break;
        }
        if line.contains("Informational, no response required") {
            break;
        }
    }
    Some(Row {
        channel: String::new(),
        last_from: sender.to_string(),
        subject,
        waiting_on,
    })
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
