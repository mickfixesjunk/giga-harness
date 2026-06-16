//! `giga sweep` — tabulate every channel's last message + WAITING ON tag.
//!
//! Replaces the project-specific `channel-state.sh` script. Run from
//! the coordinator's terminal every few minutes to spot stalled channels.

use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::foundation::frame;

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
        let row = match frame::last_header_block(&body) {
            Some(lf) => Row {
                channel: ch.file.clone(),
                last_from: lf.header.sender.clone(),
                subject: lf.header.subject.clone(),
                waiting_on: lf.waiting_on().map(|s| s.to_string()),
            },
            None => Row {
                channel: ch.file.clone(),
                last_from: "(empty)".into(),
                subject: "—".into(),
                waiting_on: None,
            },
        };
        rows.push(row);
    }

    let filtered: Vec<&Row> = if let Some(who) = owed_by_filter {
        rows.iter()
            .filter(|r| r.waiting_on.as_deref() == Some(who))
            .collect()
    } else {
        rows.iter().collect()
    };

    println!(
        "{:<35} {:<15} {:<50} {}",
        "channel", "last_from", "subject", "waiting_on"
    );
    println!("{}", "-".repeat(120));
    for r in &filtered {
        let wait = r
            .waiting_on
            .clone()
            .unwrap_or_else(|| "informational".into());
        let subj = trunc(&r.subject, 48);
        println!(
            "{:<35} {:<15} {:<50} {}",
            r.channel, r.last_from, subj, wait
        );
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

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
