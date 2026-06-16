# `src/runtime/` — the per-runtime abstraction

A swarm can mix Claude Code, Codex CLI, and Antigravity (`agy`) agents on the same
channels. This subsystem abstracts the three places giga's behavior varies per
runtime behind a single `Runtime` enum, with one submodule per concrete runtime.

## What varies per runtime

Per the module doc, exactly three things differ between runtimes (everything else
— the AGENTS.md filename, the channel conventions, the cursor model — is
universal):

1. **Launch command default** — `claude -c` / `codex` / `agy`.
2. **The Session Start snippet** baked into the generated `AGENTS.md` (Claude's
   `Monitor` tool vs an `agy` background `run_command` vs a separate Codex
   bridge pane).
3. **Watcher delivery mode** — default stdout / `--agy` exit-on-WAITING-ON /
   `--codex` envelope bridge — and the resulting **pane count** (1 for
   claude/agy, 2 for codex: the CLI + a `giga watch --codex` bridge pane).

## The `Runtime` enum (`mod.rs`)

```rust
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum Runtime { #[default] Claude, Codex, Agy }
```

It is a **`Copy` enum**, intentionally — not a `Box<dyn RuntimeTrait>`. The
per-runtime variation is a small, closed set of pure lookups (a snippet string,
an intro string, a session-log path), so the methods just `match self` and
dispatch into the `claude`/`codex`/`agy` submodule constants. A `Copy` value
threads cheaply through `Config::agent_runtime`, `scaffold`, and `mobility`
without lifetimes or allocation; trait objects would buy nothing here and cost
indirection. The deliberate non-conversion to trait objects is the design.

Methods on `Runtime`:

- `as_str(&self) -> &'static str` — `"claude"` / `"codex"` / `"agy"`.
- `parse(s) -> Option<Self>` — case-insensitive; accepts `"agy"` or
  `"antigravity"`.
- `needs_bridge_pane(&self) -> bool` — true only for `Codex`.
- `session_start_snippet(&self) -> &'static str` — dispatches to the submodule
  `SESSION_START` const.
- `launch_intro_prompt(&self) -> &'static str` — dispatches to the submodule
  `INTRO` const.
- `session_log(&self, workdir) -> Option<PathBuf>` — dispatches to the
  submodule `session_log`.

The dispatch pattern (representative):

```rust
pub fn session_start_snippet(&self) -> &'static str {
    match self {
        Runtime::Claude => claude::SESSION_START,
        Runtime::Codex  => codex::SESSION_START,
        Runtime::Agy    => agy::SESSION_START,
    }
}
```

`mod.rs` also holds the shared `pub(crate) fn most_recent_jsonl(dir)` used by all
three session-log locators.

## The per-runtime submodules

Each of [`claude`](./claude.rs), [`codex`](./codex.rs), [`agy`](./agy.rs) exposes
the same shape:

- `pub const SESSION_START: &str` — `include_str!`'d from
  `templates/runtimes/<rt>.md` (the AGENTS.md Session Start protocol).
- `pub const INTRO: &str` — `include_str!`'d from
  `templates/runtimes/<rt>-intro.md` (the launch intro prompt).
- `pub fn session_log(home, workdir) -> Option<PathBuf>` — locate the prior
  session log so `takeover` can point a fresh CLI at it. Claude encodes the
  workdir by replacing both `/` **and** `.` with `-` under
  `~/.claude/projects/<encoded>/`; Codex uses a different encoding and is
  best-effort; Agy ignores the workdir entirely and returns the global
  `~/.gemini/antigravity-cli/history.jsonl` (it keeps no per-cwd logs).

## How it's used

- `config::resolve::agent_runtime` returns a `Runtime` (priority agent →
  project → `Claude`).
- `scaffold::render` injects `session_start_snippet()` into the agent's
  AGENTS.md; `scaffold::launch` uses `launch_intro_prompt()` + `needs_bridge_pane()`
  to build panes.
- `mobility::takeover` parses the target runtime, re-renders AGENTS.md for it, and
  uses `session_log()` to locate the prior log.

## Cross-references

- [`../README.md`](../README.md) — the `src/` layered map.
- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — §5 (Config and runtimes;
  the per-runtime launch/watcher/pane table).
- [`../scaffold/README.md`](../scaffold/README.md) — where the snippet + intro
  are consumed.
- [`../mobility/README.md`](../mobility/README.md) — `takeover` flips the runtime.
- `templates/runtimes/` — the source text baked into the consts here.
