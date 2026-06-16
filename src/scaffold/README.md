# `src/scaffold/` — config → on-disk artifacts → running terminals

Turns a swarm config into the files agents read (`giga init`) and the terminals
they run in (`giga launch`). The defining split here is **effects vs text**: the
filesystem mutations live in `init`, the pure AGENTS.md / channel-header *text
generation* lives in `render`.

## Modules (`mod.rs`)

`pub mod`: [`init`](./init.rs), [`render`](./render.rs), [`launch`](./launch.rs),
[`templates`](./templates.rs), [`terminal`](./terminal/) (a sub-tree of backends).

## init vs render — the effects/text split

- [`render.rs`](./render.rs) is **side-effect free** — every function returns a
  `String`. `render_agent_claudemd(cfg, agent, config_dir, config_path)` builds
  an agent's full AGENTS.md body; `render_channel_header(cfg, ch)` builds a
  channel file's convention header. Internals: `inject_session_start` (swaps the
  `{{SESSION_START}}` placeholder — or a legacy `## Session Start` section — for
  the runtime's snippet), `render_swarm_boss_section` (the sync+merger Monitor
  lines for a boss), `prepend_header` (identity callout + code_root note + boss
  section).
- [`init.rs`](./init.rs) owns **all the effects**. `run(config_path)` /
  `run_with(config_path, do_trust)`: host-filter the agents, mkdir the inbox
  dirs, write each channel's header (calling `render::render_channel_header`,
  only when absent/empty), write each agent's AGENTS.md (calling
  `render::render_agent_claudemd`, **always** re-rendered so config changes
  propagate), scaffold codex bridge dirs, `#[cfg(unix)]`-symlink the config into
  workdirs, copy `HANDOVER.md` once, optionally `trust::pre_trust`, and
  `registry::upsert`.

So: `render` is the part `mobility::takeover` reuses (it just wants the text);
`init` is the part that touches disk.

## launch (`giga launch`)

`launch::run(LaunchArgs { config_path, skip_init, dry_run, only, new_window,
terminal, stagger_per_agent_seconds, ui, ui_port })` translates the config into
`terminal::Pane`s and spawns them. It optionally runs `init` first, narrows by
`--only` and `this_host`, then `flat_map`s agents into panes:
`intro_for_agent(intro, agent)` builds the per-agent identity preamble;
`default_cmd_for_runtime` dispatches per `Runtime` to `default_cmd_claude` /
`default_cmd_agy_interactive` / `default_cmd_tty_only` (codex). Codex agents get
**two** panes (a `<agent>-bridge` running `giga watch --codex` + a `<agent>-cli`).
`should_spawn_daemons_v2` decides whether to add `giga-sync`/`giga-merger` panes
(skipped when a local `swarm_boss` arms them as Monitors instead); `--ui` may add
a `giga-ui` pane. Everything dispatches to `terminal::launch`.

## templates (`templates.rs`)

`include_str!`-baked AGENTS.md building blocks: `pub const AGENT_STUB` (the
`giga add-agent` stub, placeholders `{{AGENT}}`/`{{ROLE}}`/`{{PEERS}}`/
`{{WATCHER}}`/`{{CONVENTION}}`), `pub const WATCHER` (the watcher-arming block),
`pub const CONVENTION` (the message-convention block). Edit the underlying `.md`
and recompile.

## terminal — `TerminalBackend` + the backend files

`terminal/mod.rs` defines the multiplexer abstraction:

```rust
pub trait TerminalBackend {
    fn name(&self) -> &'static str;
    fn launch(&self, panes: &[Pane], session: &LaunchSession) -> Result<()>;
}

pub enum Multiplexer { WindowsTerminal, Tmux, MacTerminal, None }
pub struct Pane { pub title, pub cwd, pub cmd, pub platform, pub admin }
pub struct LaunchSession { pub session_name, pub incremental, pub new_window,
                           pub stagger_seconds }
```

`detect()` auto-selects a `Multiplexer` (pure precedence in `decide_multiplexer`:
in-tmux beats wt, else wt, else tmux, else None); `parse_override(s)` handles the
`--terminal` flag; `launch(...)` resolves a backend via `backend_for(mux)` and
calls it. One backend per file:

| File | Struct | `name()` |
|---|---|---|
| [`terminal/wt.rs`](./terminal/wt.rs) | `WindowsTerminal` | `"windows-terminal"` |
| [`terminal/tmux.rs`](./terminal/tmux.rs) | `Tmux` | `"tmux"` |
| [`terminal/mac.rs`](./terminal/mac.rs) | `MacTerminal` | `"mac-terminal"` |
| [`terminal/print.rs`](./terminal/print.rs) | `Print` | `"print"` (fallback) |

[`terminal/script.rs`](./terminal/script.rs) holds the helpers shared by the wt
and mac backends: `stagger_sleep`, `sanitize_for_filename`, `chmod_executable` —
both backends route WSL panes through a temp `.sh` script (`bash -li <script>`)
to dodge the WT→wsl→bash quoting gauntlet.

## Cross-references

- [`../render`](./render.rs) is reused by
  [`../mobility/README.md`](../mobility/README.md) (`takeover` re-renders
  AGENTS.md via `scaffold::render::render_agent_claudemd`).
- [`../runtime/README.md`](../runtime/README.md) — the snippet/intro/pane-count
  source `render` and `launch` consume.
- [`../config/README.md`](../config/README.md) — `channel_path`,
  `agent_runtime`, `channel_is_local` drive the host filtering.
- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — §4 (command lifecycle:
  init → launch), §5 (swarm boss daemon panes).
- `templates/` — the source text behind `templates.rs` and the runtime snippets.
