//! Read-only process introspection — what's actually running on
//! this machine right now.
//!
//! Two sources for v0.6.36 Phase E:
//!   * `tmux list-sessions` + `tmux list-windows` — every agent
//!     pane that giga launched lives in a tmux session named
//!     `giga-<swarm>` with windows named after the agent slug
//!     (or `<slug>-bridge` / `<slug>-cli` for codex agents).
//!   * `ps -eo pid,args` — finds `giga watch --as <slug>` Monitor
//!     processes that aren't tied to a tmux pane (e.g. the bridge
//!     sidecar running under a Claude Monitor task).
//!
//! Both are shelled out — `tmux` and `ps` are universally present
//! on the operator's machine and the data is small enough that we
//! don't need a procfs crate or libtmux bindings.

use serde::Serialize;
use std::process::Command;

/// A snapshot of every relevant process the dashboard cares about.
#[derive(Debug, Serialize)]
pub struct ProcessSnapshot {
    pub tmux: Vec<TmuxSession>,
    pub watchers: Vec<WatcherProcess>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TmuxSession {
    /// Session name as `tmux list-sessions` reports it. For
    /// giga-launched swarms this is `giga-<swarm>`.
    pub name: String,
    pub windows: Vec<TmuxWindow>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TmuxWindow {
    pub name: String,
    /// PID of the active pane in this window (PID 1 of the pane's
    /// process group). Useful for tying a window back to a ps row.
    pub pane_pid: Option<u32>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct WatcherProcess {
    pub pid: u32,
    /// Agent slug extracted from `--as <slug>` on the cmdline.
    pub agent: String,
    /// "claude" (default), "codex" (--codex flag), or "agy" (--agy flag).
    pub runtime: String,
}

pub fn snapshot() -> ProcessSnapshot {
    ProcessSnapshot {
        tmux: tmux_sessions().unwrap_or_default(),
        watchers: watcher_processes().unwrap_or_default(),
    }
}

/// Enumerate every tmux session + its windows. Returns `Err` (which
/// callers usually convert to empty) when tmux isn't on PATH or no
/// server is running.
pub fn tmux_sessions() -> Result<Vec<TmuxSession>, std::io::Error> {
    let sessions_raw = run(&["tmux", "list-sessions", "-F", "#{session_name}"])?;
    let mut sessions = Vec::new();
    for name in sessions_raw.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let windows_raw = run(&[
            "tmux",
            "list-windows",
            "-t",
            name,
            "-F",
            "#{window_name}\t#{pane_pid}",
        ])
        .unwrap_or_default();
        let windows = parse_tmux_windows(&windows_raw);
        sessions.push(TmuxSession {
            name: name.to_string(),
            windows,
        });
    }
    Ok(sessions)
}

/// Enumerate `giga watch` Monitor watcher processes. Anchors on
/// `giga watch --as <slug>` so the scan picks up both sync
/// (`giga watch --as alice`) and codex-runtime
/// (`giga watch --as alice --codex`) shapes.
pub fn watcher_processes() -> Result<Vec<WatcherProcess>, std::io::Error> {
    let raw = run(&["ps", "-eo", "pid=,args="])?;
    Ok(parse_ps_output(&raw))
}

fn run(argv: &[&str]) -> Result<String, std::io::Error> {
    let out = Command::new(argv[0]).args(&argv[1..]).output()?;
    if !out.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "{} exited with status {}: {}",
                argv[0],
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn parse_tmux_windows(raw: &str) -> Vec<TmuxWindow> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let mut parts = line.splitn(2, '\t');
        let name = parts.next().unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        let pid = parts.next().unwrap_or("").trim().parse::<u32>().ok();
        out.push(TmuxWindow {
            name: name.to_string(),
            pane_pid: pid,
        });
    }
    out
}

fn parse_ps_output(raw: &str) -> Vec<WatcherProcess> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim_start();
        let mut parts = line.splitn(2, char::is_whitespace);
        let pid_str = parts.next().unwrap_or("");
        let args = parts.next().unwrap_or("");
        if !args.contains("giga watch") {
            continue;
        }
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let agent = match extract_as_slug(args) {
            Some(s) => s,
            None => continue,
        };
        let runtime = if args.contains(" --codex") {
            "codex"
        } else if args.contains(" --agy") {
            "agy"
        } else {
            "claude"
        };
        out.push(WatcherProcess {
            pid,
            agent,
            runtime: runtime.to_string(),
        });
    }
    out
}

fn extract_as_slug(args: &str) -> Option<String> {
    // Find `--as ` then take the next valid-slug-char run. Agent
    // slugs are `[a-zA-Z0-9_-]+`; stop at anything else so we strip
    // trailing shell metacharacters (quotes, redirects, pipes) that
    // bash-wrapped giga watch invocations leave in the cmdline —
    // e.g. `eval 'giga watch --as superdeduper' < /dev/null` would
    // otherwise yield the slug `superdeduper'`.
    let needle = "--as ";
    let idx = args.find(needle)?;
    let rest = &args[idx + needle.len()..];
    let token: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tmux_windows_extracts_name_and_pid() {
        let raw = "superdeduper\t1234\nclaude-cli\t9876\n";
        let parsed = parse_tmux_windows(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "superdeduper");
        assert_eq!(parsed[0].pane_pid, Some(1234));
        assert_eq!(parsed[1].name, "claude-cli");
        assert_eq!(parsed[1].pane_pid, Some(9876));
    }

    #[test]
    fn parse_tmux_windows_handles_missing_pid_column() {
        let raw = "name-only\n";
        let parsed = parse_tmux_windows(raw);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "name-only");
        assert_eq!(parsed[0].pane_pid, None);
    }

    #[test]
    fn parse_tmux_windows_skips_blank_lines() {
        let raw = "\n\nfoo\t1\n\nbar\t2\n";
        let parsed = parse_tmux_windows(raw);
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_ps_picks_up_giga_watch_claude_default() {
        let raw = "  4640 giga watch --as airflow\n";
        let parsed = parse_ps_output(raw);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].pid, 4640);
        assert_eq!(parsed[0].agent, "airflow");
        assert_eq!(parsed[0].runtime, "claude");
    }

    #[test]
    fn parse_ps_distinguishes_codex_and_agy_runtimes() {
        let raw = "\
  4640 giga watch --as airflow --agy\n\
  7292 giga watch --as superdeduper\n\
 65255 giga watch --as codex-review --codex\n";
        let parsed = parse_ps_output(raw);
        assert_eq!(parsed.len(), 3);
        assert!(parsed.iter().any(|p| p.agent == "airflow" && p.runtime == "agy"));
        assert!(parsed.iter().any(|p| p.agent == "superdeduper" && p.runtime == "claude"));
        assert!(parsed.iter().any(|p| p.agent == "codex-review" && p.runtime == "codex"));
    }

    #[test]
    fn parse_ps_ignores_non_giga_watch_rows() {
        let raw = "\
  1234 sshd: neomatrix [priv]\n\
  5678 /usr/bin/python3 -m something\n";
        assert!(parse_ps_output(raw).is_empty());
    }

    #[test]
    fn parse_ps_ignores_giga_watch_without_as_slug() {
        let raw = "  4640 giga watch --some-other-flag\n";
        assert!(parse_ps_output(raw).is_empty());
    }

    #[test]
    fn extract_as_slug_picks_token_after_as_flag() {
        assert_eq!(extract_as_slug("giga watch --as alice"), Some("alice".into()));
        assert_eq!(
            extract_as_slug("giga watch --as code-review-bridge --codex"),
            Some("code-review-bridge".into())
        );
    }

    #[test]
    fn extract_as_slug_returns_none_when_flag_missing() {
        assert_eq!(extract_as_slug("giga watch"), None);
        assert_eq!(extract_as_slug("--as-is-a-prefix-only"), None);
    }

    /// Regression for the bash-wrapped invocation discovered on
    /// the live superdeduper swarm: `eval 'giga watch --as
    /// superdeduper' < /dev/null` produced `superdeduper'` instead
    /// of `superdeduper`. The slug extractor must stop at the
    /// single-quote.
    #[test]
    fn extract_as_slug_strips_trailing_shell_metacharacters() {
        assert_eq!(
            extract_as_slug("eval 'giga watch --as superdeduper' < /dev/null"),
            Some("superdeduper".into())
        );
        assert_eq!(
            extract_as_slug("bash -c \"giga watch --as alice\""),
            Some("alice".into())
        );
        // Mid-args pipeline: `--as foo | grep ...`.
        assert_eq!(
            extract_as_slug("giga watch --as foo | grep x"),
            Some("foo".into())
        );
    }
}
