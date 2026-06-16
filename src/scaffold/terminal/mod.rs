//! Cross-platform terminal multiplexer detection + spawn.
//!
//! Strategy (in priority order, auto-detected):
//!   1. Windows Terminal (`wt.exe`) — best UX on Windows; one window
//!      with N tabs, mixed wsl/windows panes via `-p` profiles.
//!   2. tmux — Linux fallback; one session, N windows.
//!   3. None — fall back to printing the per-agent commands so the
//!      user can paste them into separate terminals manually.
//!
//! `MacTerminal` opens one Terminal.app window per agent via
//! `osascript`. Opt-in only (`giga launch --terminal mac-terminal`);
//! never auto-detected so existing tmux users on macOS keep their
//! current behavior.
//!
//! Each strategy is a [`TerminalBackend`] implementor living in its own
//! submodule (`wt`, `tmux`, `mac`, `print`). [`detect`] /
//! [`parse_override`] map the legacy `Multiplexer` enum + `--terminal`
//! string onto a boxed backend; [`launch`] preserves the old free-fn
//! call shape so callers (and tests) keep working unchanged.

mod mac;
mod print;
mod script;
mod tmux;
mod wt;

use anyhow::Result;
use which::which;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    WindowsTerminal,
    Tmux,
    MacTerminal,
    None,
}

/// Per-launch context shared by every backend. Each backend reads only
/// the fields it needs (e.g. `print` ignores all of them; `tmux` ignores
/// `new_window`); the struct carries the union so the trait method has a
/// single, stable signature.
pub struct LaunchSession {
    /// Multiplexer session / window name (`giga-<project>`).
    pub session_name: String,
    /// `--only` launch: attach to an existing session and add to it
    /// rather than rebuild from scratch. Only meaningful for tmux.
    pub incremental: bool,
    /// Force a fresh wt window (`wt -w new`) instead of targeting the
    /// project's named window. Only meaningful for Windows Terminal.
    pub new_window: bool,
    /// Sleep this many seconds between starting each pane's command.
    pub stagger_seconds: u64,
}

/// A terminal launcher. One implementor per strategy
/// (`wt` / `tmux` / `mac` / `print`).
pub trait TerminalBackend {
    /// Stable identifier for diagnostics. Unused by the launch plan
    /// output (which prints the `Multiplexer` enum), but handy for tests
    /// and future logging.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    /// Spawn one terminal entity per pane per this backend's strategy.
    fn launch(&self, panes: &[Pane], session: &LaunchSession) -> Result<()>;
}

pub fn detect() -> Multiplexer {
    let in_tmux = std::env::var("TMUX").is_ok();
    let tmux_avail = which("tmux").is_ok();
    // Inside WSL, `wt.exe` is on PATH via Windows interop.
    let wt_avail = which("wt.exe").is_ok() || which("wt").is_ok();
    decide_multiplexer(in_tmux, tmux_avail, wt_avail)
}

/// Pure precedence logic for `detect()`. Extracted for testing.
///
/// v0.6.25: if `$TMUX` is set the operator is running giga from
/// inside a tmux session; spawning into wt.exe in that case
/// surprises them with a fresh Windows Terminal window instead of
/// adding agents to their current tmux session. Treat `$TMUX` as a
/// strong hint and prefer tmux when it's available, even if wt.exe
/// is on PATH (which it always is in WSL).
fn decide_multiplexer(in_tmux: bool, tmux_avail: bool, wt_avail: bool) -> Multiplexer {
    if in_tmux && tmux_avail {
        return Multiplexer::Tmux;
    }
    if wt_avail {
        return Multiplexer::WindowsTerminal;
    }
    if tmux_avail {
        return Multiplexer::Tmux;
    }
    Multiplexer::None
}

/// Parse a `--terminal` flag value. `auto` means use `detect()`.
/// Returns None for unknown values so the caller can surface a
/// helpful error.
pub fn parse_override(s: &str) -> Option<Multiplexer> {
    match s {
        "auto" => Some(detect()),
        "wt" | "windows-terminal" => Some(Multiplexer::WindowsTerminal),
        "tmux" => Some(Multiplexer::Tmux),
        "mac-terminal" | "mac" => Some(Multiplexer::MacTerminal),
        "print" | "none" => Some(Multiplexer::None),
        _ => None,
    }
}

/// Map a `Multiplexer` variant to its boxed backend.
fn backend_for(mux: Multiplexer) -> Box<dyn TerminalBackend> {
    match mux {
        Multiplexer::WindowsTerminal => Box::new(wt::WindowsTerminal),
        Multiplexer::Tmux => Box::new(tmux::Tmux),
        Multiplexer::MacTerminal => Box::new(mac::MacTerminal),
        Multiplexer::None => Box::new(print::Print),
    }
}

pub struct Pane {
    pub title: String,
    /// Working directory before the command runs.
    pub cwd: String,
    /// Shell command to execute. Already shell-escaped where needed.
    pub cmd: String,
    /// "wsl" or "windows" — affects which wt profile we pick.
    pub platform: String,
    /// Request UAC elevation for this tab (Windows Terminal only).
    pub admin: bool,
}

pub fn launch(
    mux: Multiplexer,
    panes: &[Pane],
    session_name: &str,
    incremental: bool,
    new_window: bool,
    stagger_seconds: u64,
) -> Result<()> {
    // wt.exe's `--window <name>` flag already does the right thing
    // for the default case: reuse the existing window with that
    // name (adds tabs) or create one if absent. `new_window`
    // overrides that with `-w new` to force a fresh wt window —
    // matters when the original launch window has been torn up
    // (tabs dragged into separate windows) and the name no longer
    // points anywhere useful. The incremental distinction only
    // matters for tmux.
    //
    // v0.6.19: `stagger_seconds` paces per-pane spawning so a
    // large swarm doesn't trigger 17 simultaneous `claude` first
    // turns → TPM-limit storm. Default 0 (current behavior); pass
    // 5-15s for 10+ agent swarms.
    let session = LaunchSession {
        session_name: session_name.to_string(),
        incremental,
        new_window,
        stagger_seconds,
    };
    backend_for(mux).launch(panes, &session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_override_accepts_canonical_names() {
        assert_eq!(parse_override("tmux"), Some(Multiplexer::Tmux));
        assert_eq!(parse_override("wt"), Some(Multiplexer::WindowsTerminal));
        assert_eq!(
            parse_override("mac-terminal"),
            Some(Multiplexer::MacTerminal)
        );
        assert_eq!(parse_override("print"), Some(Multiplexer::None));
    }

    #[test]
    fn parse_override_accepts_aliases() {
        // `windows-terminal` is the long-form alias for `wt`.
        assert_eq!(
            parse_override("windows-terminal"),
            Some(Multiplexer::WindowsTerminal)
        );
        // `mac` is the short alias for `mac-terminal`.
        assert_eq!(parse_override("mac"), Some(Multiplexer::MacTerminal));
        // `none` is the alias for `print`.
        assert_eq!(parse_override("none"), Some(Multiplexer::None));
    }

    #[test]
    fn decide_multiplexer_prefers_tmux_when_inside_tmux_session() {
        // Operator launched giga from inside an active tmux session:
        // even though wt.exe is on PATH (always true in WSL), they
        // want new panes added to their current tmux session, not a
        // surprise wt window.
        assert_eq!(
            decide_multiplexer(true, true, true),
            Multiplexer::Tmux,
            "in-tmux should beat wt.exe"
        );
    }

    #[test]
    fn decide_multiplexer_prefers_wt_when_not_inside_tmux() {
        // No TMUX env: WSL default — wt.exe wins (historical
        // behavior).
        assert_eq!(
            decide_multiplexer(false, true, true),
            Multiplexer::WindowsTerminal,
        );
    }

    #[test]
    fn decide_multiplexer_falls_through_to_tmux_without_wt() {
        // Pure-Linux host: no wt.exe, tmux installed.
        assert_eq!(decide_multiplexer(false, true, false), Multiplexer::Tmux,);
    }

    #[test]
    fn decide_multiplexer_returns_none_when_neither_available() {
        assert_eq!(decide_multiplexer(false, false, false), Multiplexer::None,);
    }

    #[test]
    fn decide_multiplexer_ignores_in_tmux_when_tmux_missing() {
        // Pathological: TMUX env set but tmux binary not on PATH.
        // Fall through to wt.exe if present.
        assert_eq!(
            decide_multiplexer(true, false, true),
            Multiplexer::WindowsTerminal,
        );
    }

    #[test]
    fn parse_override_auto_returns_detect_result() {
        // `auto` defers to `detect()`. We can't assert which variant
        // comes back (depends on what's installed on the test host),
        // but it should always return Some.
        assert!(parse_override("auto").is_some());
    }

    #[test]
    fn parse_override_rejects_unknown_value() {
        assert_eq!(parse_override("kitty"), None);
        assert_eq!(parse_override(""), None);
        assert_eq!(parse_override("TMUX"), None, "case-sensitive");
    }

    #[test]
    fn backend_for_maps_each_variant_to_its_named_backend() {
        assert_eq!(
            backend_for(Multiplexer::WindowsTerminal).name(),
            "windows-terminal"
        );
        assert_eq!(backend_for(Multiplexer::Tmux).name(), "tmux");
        assert_eq!(backend_for(Multiplexer::MacTerminal).name(), "mac-terminal");
        assert_eq!(backend_for(Multiplexer::None).name(), "print");
    }
}
