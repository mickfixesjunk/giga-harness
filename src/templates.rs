//! Compiled-in agent CLAUDE.md templates.
//!
//! The source `.md` files live in-repo under `templates/` and are baked into
//! the binary with `include_str!`, so giga has NO runtime dependency on any
//! external configs repo. To change the generated text, edit the `.md` files.
//!
//! Shared prose (the Monitor-watcher warning, the message convention) lives in
//! `templates/partials/` and is reused by both `giga add-agent` (the stub) and
//! `giga init` (auto-generated CLAUDE.md) so the two never drift apart.

/// Full `giga add-agent` stub written to `agents/<name>.md`.
/// Placeholders: `{{AGENT}}`, `{{ROLE}}`, `{{PEERS}}`, `{{WATCHER}}`, `{{CONVENTION}}`.
pub const AGENT_STUB: &str = include_str!("../templates/agent.md");

/// Shared watcher-arming block + the CRITICAL "Monitor TOOL, not Bash" warning.
/// Placeholder: `{{AGENT}}`.
pub const WATCHER: &str = include_str!("../templates/partials/watcher.md");

/// Shared message-convention block (`WAITING ON:` / `Informational`). No placeholders.
pub const CONVENTION: &str = include_str!("../templates/partials/convention.md");
