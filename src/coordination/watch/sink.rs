//! Terminal notification sinks for the watcher.
//!
//! The watcher's poll/buffer/stagger machinery is identical across the
//! three watch modes; only the FINAL emit differs:
//!
//! * **Default (Claude):** `println!` a line for the Monitor tool.
//! * **`--agy`:** same line + force-flush, and exit-0 when the latest
//!   frame is `WAITING ON: <me>` (fires AGY's task-completion wakeup).
//! * **`--codex`:** write a JSON envelope into the codex inbox dir.
//!
//! [`NotificationSink`] is the seam: `run_multi`/`run_single` build the
//! ready line, then hand it to the sink. The mode→sink mapping replaces
//! the scattered `match mode` / `matches!(mode, …)` branches that used to
//! live in the emit path. None of the buffering, stagger, archive,
//! rearm, stale-wait, or cursor logic moves — only the emit + the
//! agy-exit predicate.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::WatchMode;

pub trait NotificationSink {
    /// Emit one ready notification line (already formatted "inbox <ch>: <line>").
    fn deliver(&mut self, line: &str);
    /// Force-flush after a batch (agy needs this; stdout no-op-ish).
    fn flush(&mut self) {}
    /// Whether this sink should exit-0 when the latest frame is WAITING ON me.
    fn exit_on_waiting_on_me(&self) -> bool {
        false
    }
    /// Supply the per-delivery context the codex envelope needs (channel
    /// name, channel path, byte offset). Called immediately before each
    /// `deliver`. Default no-op: the stdout/agy sinks don't need it.
    fn prime(&mut self, _channel: &str, _path: &Path, _offset: u64) {}
}

/// Default / Claude mode: a plain stdout line per notification.
pub struct StdoutSink;

impl NotificationSink for StdoutSink {
    fn deliver(&mut self, line: &str) {
        println!("{line}");
    }
    fn flush(&mut self) {
        let _ = std::io::stdout().flush();
    }
}

/// `--agy` mode: identical stdout deliver+flush as [`StdoutSink`], but
/// signals exit-0 when the latest frame is WAITING ON me.
pub struct AgySink;

impl NotificationSink for AgySink {
    fn deliver(&mut self, line: &str) {
        println!("{line}");
    }
    fn flush(&mut self) {
        let _ = std::io::stdout().flush();
    }
    fn exit_on_waiting_on_me(&self) -> bool {
        true
    }
}

/// `--codex` mode: write a JSON envelope into the per-agent codex inbox
/// dir. The codex CLI picks it up, surfaces it to the agent, and writes
/// a receipt to the outbox.
///
/// `deliver` reconstructs the same envelope `text` the inline match arm
/// used to build; the per-delivery context (channel / path / offset) is
/// supplied by [`CodexSink::prime`] just before each `deliver`, so the
/// trait's single-`&str` `deliver` signature is preserved while the
/// envelope keeps its exact prior shape.
pub struct CodexSink {
    pub dir: PathBuf,
    pub me: String,
    pub project: String,
    // Per-delivery context, set by `prime` before each `deliver`.
    channel: String,
    path: PathBuf,
    offset: u64,
}

impl CodexSink {
    pub fn new(dir: PathBuf, me: String, project: String) -> Self {
        CodexSink {
            dir,
            me,
            project,
            channel: String::new(),
            path: PathBuf::new(),
            offset: 0,
        }
    }
}

impl NotificationSink for CodexSink {
    /// Supply the per-delivery context the codex envelope needs. Called
    /// by the watch loop immediately before `deliver`; mirrors exactly
    /// what the old inline `WatchMode::Codex` arm had in scope.
    fn prime(&mut self, channel: &str, path: &Path, offset: u64) {
        self.channel = channel.to_string();
        self.path = path.to_path_buf();
        self.offset = offset;
    }

    fn deliver(&mut self, header_line: &str) {
        // Same envelope text the inline match arm produced.
        let text = format!(
            "Giga inbox notification for `{me}`.\n\n\
             Channel: {channel}\n\
             Path: {path}\n\
             Header: {header}\n\n\
             Read the channel file, follow your agent instructions, \
             and respond via `giga post` if the message requires action.",
            me = self.me,
            channel = self.channel,
            path = self.path.display(),
            header = header_line,
        );
        if let Err(e) = crate::coordination::codex_channel::write_envelope(
            &self.dir,
            &self.project,
            &self.me,
            &self.channel,
            self.offset,
            &text,
        ) {
            eprintln!("watch [codex]: envelope write failed: {e:#}");
        }
    }
}

/// Build the sink for a watch mode. The mode→sink mapping is the single
/// place the three modes diverge; everything upstream is shared.
///
/// * `Default` → [`StdoutSink`]
/// * `Agy`     → [`AgySink`]
/// * `Codex`   → [`CodexSink`] (requires `codex_inbox` + `project`; the
///   caller validates `CODEX_CHANNEL_DIR` up-front and only passes
///   `Some` here in `--codex` mode).
pub fn sink_for(
    mode: WatchMode,
    me: &str,
    codex_inbox: Option<PathBuf>,
    project: &str,
) -> Box<dyn NotificationSink> {
    match mode {
        WatchMode::Default => Box::new(StdoutSink),
        WatchMode::Agy => Box::new(AgySink),
        WatchMode::Codex => {
            // Codex mode is only selected when run_multi has already
            // resolved + validated the inbox dir, so this is always Some.
            let dir = codex_inbox.expect("codex mode requires a resolved inbox dir");
            Box::new(CodexSink::new(dir, me.to_string(), project.to_string()))
        }
    }
}
