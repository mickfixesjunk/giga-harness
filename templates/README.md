# `templates/` — generation payloads (operator/agent docs, runtime intros, partials, dashboard)

The static text and markup `giga` renders into agents, operators, and the web dashboard. Every file here is baked into the `giga` binary at compile time via `include_str!` (or `include_bytes!` for the brand icon), so the running binary has **zero runtime dependency** on any external configs repo — to change generated text, edit these files and rebuild.

## Role in the system

When `giga` scaffolds a swarm (`init`, `add-agent`), boots a CLI (`launch`), prints operator help (`claude-operator`), or serves the dashboard (`ui`), the content it emits originates here. The files split into four roles: (1) `CLAUDE_OPERATOR.md` — the operator command reference; (2) `agent.md` + `partials/` — the building blocks `add-agent` assembles into a per-agent `AGENTS.md` via placeholder substitution; (3) `runtimes/<rt>.md` and `runtimes/<rt>-intro.md` — the runtime-aware Session Start sections (`init`) and one-shot launch prompts (`launch`) for each of the three runtimes (Claude / Codex / Antigravity); (4) `ui/dashboard.html` — the entire single-file web dashboard served by `giga ui`. The `runtimes/` files encode the three watcher-arming protocols (Claude `Monitor` tool, agy `run_command` background task, codex sidecar bridge pane), which is the core behavioral knowledge differentiating the runtimes.

## File index

| File | Lines (approx) | Purpose |
|---|---|---|
| `CLAUDE_OPERATOR.md` | 223 | Operator command reference printed by `giga claude-operator`. |
| `agent.md` | 34 | Per-agent `AGENTS.md` stub written by `giga add-agent`. |
| `partials/convention.md` | 6 | Shared message-closing convention block (`WAITING ON:` / Informational). |
| `partials/watcher.md` | 20 | Shared Claude `Monitor`-watcher arming block + "not Bash" warning. |
| `runtimes/claude.md` | 20 | Claude Session Start section (`Monitor` tool). |
| `runtimes/claude-intro.md` | 1 | Claude launch-time opening prompt. |
| `runtimes/agy.md` | 76 | Antigravity Session Start section (`run_command` + sweep cron). |
| `runtimes/agy-intro.md` | 1 | Antigravity launch-time opening prompt. |
| `runtimes/codex.md` | 59 | Codex Session Start section (bridge pane + post-after-command rule). |
| `runtimes/codex-intro.md` | 1 | Codex launch-time opening prompt. |
| `ui/dashboard.html` | 1086 | The entire `giga ui` web dashboard (HTML + inline CSS + vanilla JS). |

## Files

### `CLAUDE_OPERATOR.md`

**Purpose.** The full operator command reference for driving a giga swarm: a 60-second mental model, core / scaffolding / multi-host / lifecycle command tables, the `--host` pattern, broadcast addressing semantics, the optional `swarm_boss` role, ten copy-pasteable recipes, and a "sharp edges" list. It is operator-facing (the only file in this folder that is *not* an `AGENTS.md` fragment) and carries **no `{{}}` placeholders**.

**Key items.** Bound as `const DOC` in `src/claude_operator.rs:32` via `include_str!("../templates/CLAUDE_OPERATOR.md")`. The subcommand is wired at `src/main.rs:335` (`ClaudeOperator`). The doc documents:
- The message header grammar: `===\n[<sender>] <subject> — <UTC ts>\n===`, body, then a footer of either `WAITING ON: <agent> (<what>)` or `(Informational, no response required.)`. The header separator MUST be an em-dash (` — `, U+2014); timestamps are UTC ISO-8601 ending in `Z`.
- Broadcast subject-prefix fanout (only `_*.md` channels): no-prefix / `[all]` (staggered, default), `[ack: a,b,c]` (synthesized by `giga post --to`), `[fyi]` (zero LLM cost, archived to `~/.giga/fyi-archive.<agent>.log`, synthesized by `giga post --fyi`), and `[giga-rearm]` (reserved for `giga upgrade`).
- Recipes 1–10: add-agent (local / peer), add-host, post, sweep, upgrade, teleport, broadcast-to-subset, takeover, switch (account rotation).

**Control flow.** `run()` (`src/claude_operator.rs:34`) branches on `std::io::stdout().is_terminal()`:
- **TTY path** (operator at a terminal): checks `which::which("claude")`, errors with an install hint if missing, then runs `Command::new("claude").arg("--append-system-prompt").arg(DOC)` with inherited stdio and exits with claude's status code. The doc becomes a system-prompt suffix on a fresh interactive Claude session.
- **Non-TTY path** (an agent's Bash tool, a pipe, a redirect): `print!("{DOC}")` to stdout. An agent invoking `giga claude-operator` via Bash captures the text into its context.

**Gotchas / invariants.**
- Self-described version lags the crate: the prose at line 3 says `v0.6.54` while the crate is `v0.6.55`. The doc is hand-maintained; it explicitly instructs the reader to trust `giga <subcommand> --help` over itself when they disagree (lines 3, 214).
- It flags one of its own stale claims: `giga watch`'s help text says "15s default" stagger, but the real default is 30s (line 69).
- The header separator and UTC-`Z` timestamp conventions are normative here; bodies may carry any UTF-8.

### `agent.md`

**Purpose.** The per-agent `AGENTS.md` stub written to `agents/<slug>.md` by `giga add-agent`. An intentionally minimal scaffold: title, role line, a "Session Start (do this first)" protocol, a TODO responsibilities block, a "Channels you watch" explainer, the message convention, and a pointer to `giga claude-operator` for multi-host ops. The author is expected to fill in role/responsibilities; the generated parts are deliberately thin.

**Key items.** Five placeholders:
- `{{AGENT}}` — the slug.
- `{{ROLE}}` — the role string.
- `{{PEERS}}` — comma-joined backticked peer list.
- `{{WATCHER}}` — filled with the body of `partials/watcher.md`.
- `{{CONVENTION}}` — filled with the body of `partials/convention.md`.

Consumed as `const AGENT_STUB` (`src/templates.rs:13`). Step 0 of its Session Start instructs reading `./HANDOVER.md` first if present (cross-session / cross-machine state). It claims a `~15s reread` cadence for auto-discovering newly added channels (`agent.md:26`).

**Control flow.** `render_agent_stub` in `src/add_agent.rs:535` first builds `watcher = WATCHER.replace("{{AGENT}}", &args.name)`, then chains `.replace` on `AGENT_STUB` in this exact order (`add_agent.rs:536-541`): `{{WATCHER}}` → `watcher.trim_end()`, `{{CONVENTION}}` → `CONVENTION.trim_end()`, `{{PEERS}}` → peer list, `{{ROLE}}` → role, `{{AGENT}}` → slug. **Order matters:** `{{WATCHER}}`/`{{CONVENTION}}` are filled with partial bodies that themselves contain `{{AGENT}}`, so the final `{{AGENT}}` replace must run last to catch placeholders inside the injected partials.

**Gotchas / invariants.**
- This is the **add-agent path only**. `giga init` does **not** use `agent.md` (see `src/init.rs render_agent_claudemd` — for a template-based agent it injects only the runtime Session Start snippet; for a no-template agent it builds a minimal `AGENTS.md` inline at `init.rs:392-450` using the runtime snippet + `CONVENTION` directly).
- Consequently `agent.md` hardcodes the generic Claude-style `Monitor` watcher (via `{{WATCHER}}`) and is **not runtime-aware** — a codex/agy agent created with `add-agent` gets a Claude-flavored stub until corrected by a runtime-aware `claudemd_template` plus `init`. Its Session Start text is Claude-centric.

### `partials/`

Shared prose blocks reused by both generators (`add-agent` and `init`) so the two paths never drift. Both are exposed as consts in `src/templates.rs`.

#### `partials/convention.md`

**Purpose.** The shared message-closing convention: every channel message ends with either `WAITING ON: <agent> (<what's needed>)` (a reply is expected) or `Informational, no response required.` (otherwise), warning that ambiguous closings stall the pipeline. Single source of truth so the two generators agree.

**Key items.** No placeholders — pure prose. Exposed as `const CONVENTION` (`src/templates.rs:20`). Injected via the `{{CONVENTION}}` placeholder in `agent.md` (add-agent path, `add_agent.rs:538`) and appended under a literal `## Convention` heading in the auto-generated init path (`init.rs:444-446`).

**Control flow.** Both consumers call `CONVENTION.trim_end()` before inserting, so trailing whitespace/newlines in the file don't bleed into the rendered doc. The init path pushes `## Convention\n\n` then the trimmed body then `\n\n`.

**Gotchas.** This partial writes the bare form `Informational, no response required.` (no surrounding parens), whereas `CLAUDE_OPERATOR.md` and the runtime snippets render the parenthesized `(Informational, no response required.)`. The canonical footer punctuation lives in the runtime files / operator doc; this partial carries the *rationale* (ambiguity stalls the pipeline), not the exact wire string.

#### `partials/watcher.md`

**Purpose.** The shared, **Claude-specific** watcher-arming block plus the emphatic "CRITICAL — use the `Monitor` TOOL, not Bash" warning. Tells the agent to copy the `Monitor(description, persistent:true, command:"giga watch --as {{AGENT}}")` invocation verbatim, and explains the first-arm history replay.

**Key items.** Single placeholder `{{AGENT}}`. Exposed as `const WATCHER` (`src/templates.rs:17`). It enumerates four forbidden failure modes (`watcher.md:13-16`): Bash foreground, `Bash(... run_in_background:true)`, `giga watch ... &`, and `Monitor(persistent:false)`. It calls out the dominant real-world break — a Bash-backgrounded watcher is "alive but deaf" (process alive, ZERO notifications delivered) — and tells the reader to "read this twice."

**Control flow.** Consumed **only** by the add-agent path: `add_agent.rs:535` substitutes `{{AGENT}}`, then the trimmed result is spliced into `agent.md`'s `{{WATCHER}}` slot. The init path does **not** use this partial — it substitutes the per-runtime `session_start_snippet()` instead (`runtimes/claude.md` carries the same Monitor guidance in terser form; agy/codex carry their own).

**Gotchas / invariants.** Hardcoded Claude/`Monitor` semantics — correct only for Claude agents. An agy or codex agent scaffolded by `add-agent` inherits these Claude-Monitor instructions in its stub until a runtime-aware template + `init` supersede them.

### `runtimes/`

The runtime-aware payloads. For each of the three runtimes there are two files, both owned by the `Runtime` enum in `src/runtime.rs`:

- `<rt>.md` — the **Session Start section** spliced into an agent's `AGENTS.md`. Returned by `Runtime::session_start_snippet()` (`runtime.rs:140-146`); carries the `{{AGENT}}` placeholder, substituted by `init`.
- `<rt>-intro.md` — the **one-shot opening prompt** `giga launch` injects into a freshly-spawned CLI. Returned by `Runtime::launch_intro_prompt()` (`runtime.rs:159-165`); a fixed string with no placeholders. A per-project `[project].launch_intro_prompt` in TOML overrides **all** runtimes when set.

`Runtime::watcher_invocation(slug)` (`runtime.rs:120`) returns the watcher command each `<rt>.md` references: `giga watch --as <slug>` (Claude), `... --agy` (Agy), `... --codex` (Codex). `Runtime::needs_bridge_pane()` (`runtime.rs:131`) is true **only for Codex**.

> **Hard invariant for the `-intro.md` files: no backticks.** The intro strings end up single-quoted on a `wt.exe → wsl.exe → bash` command line in `src/launch.rs` (see `launch.rs:327` and the comment at `runtime.rs:155-158`). A backtick would survive single-quoting and get shell-evaluated as command substitution, corrupting the prompt the agent sees. All three `-intro.md` files comply (plain prose, no code spans/fences). The `<rt>.md` files are **not** subject to this rule — they are written into `AGENTS.md`, never onto a shell line — so they may (and do) contain indented code blocks.

#### `runtimes/claude.md` — Claude Session Start

Arm the inbox watcher via Claude Code's `Monitor` TOOL with `persistent:true` and `command:"giga watch --as {{AGENT}}"` — **not** Bash (Bash stdout never reaches the conversation). Explains that one watcher tracks every participating channel with a `~15s reload` cadence, that the watcher replays unread history as the first notification batch before posting a one-line intro and standing by, and that `TaskStop` stops it. Begins with its own `## Session Start (do this first, every session)` heading, so consumers must not add another. Placeholder: `{{AGENT}}` (in the `Monitor` command). This is the runtime-aware analogue of `partials/watcher.md` but used by `init` rather than `add-agent`, and is terser than the partial.

#### `runtimes/claude-intro.md` — Claude launch prompt

Single paragraph. cwd holds `AGENTS.md`/`HANDOVER.md` (don't filesystem-search). **Unique to Claude:** call `TaskList` as the authoritative liveness check — it shows only the current conversation's tasks; if it returns no `Monitor` entries, every Monitor died (a WSL VM reboot can kill Monitors while Claude Code preserves chat history), so silently re-arm every Monitor per `AGENTS.md`. "Do NOT trust chat history as evidence a Monitor is alive." Then resume mid-task or follow `AGENTS.md`, and arm the watcher via the `Monitor` TOOL with `persistent:true` (not Bash, even `run_in_background`). The `TaskList` re-arm logic is Claude-specific — only Claude Code has the `Monitor`/`TaskList` primitive.

#### `runtimes/agy.md` — Antigravity Session Start

Arm the watcher via Antigravity's `run_command(Command:"giga watch --as {{AGENT}} --agy", WaitMsBeforeAsync:1000)` — the small `WaitMsBeforeAsync` makes the runtime detach it as a background task. The `--agy` flag force-flushes stdout on every print and exits cleanly with code 0 the moment a message arrives that is `WAITING ON: {{AGENT}}`, which fires agy's task-completion wakeup. When the watcher exits, re-arm it the same way. Documents:
- A **belt-and-suspenders periodic sweep** via agy's `schedule` tool (`CronExpression:"*/10 * * * *"`, running `giga sweep --owed-by {{AGENT}}`) — agy is the only runtime with this fallback, because `--agy` only wakes on direct `WAITING ON:` asks, so a fully idle agent can otherwise miss informational broadcasts.
- That `giga sweep` filters with `--owed-by <slug>`, **NOT** `--as` (which belongs to `post`/`watch`).
- The posting-back command shape (`giga post ... --as {{AGENT}} [--waiting-on <recipient>]`).
- **Closing-tag guidance:** when handing back a result the requester must act on, close with `WAITING ON: <requester>`, **not** Informational — under `--agy` an idle requester won't wake on an Informational close until the next 10-minute sweep tick.

Begins with its own `## Session Start` heading. Placeholder `{{AGENT}}` appears in the `run_command`, the cron prompt, the sweep filter, and `post --as`. Contains indented code blocks (no backticks) — fine, since this file is never a launch intro.

#### `runtimes/agy-intro.md` — Antigravity launch prompt

Single dense paragraph. cwd holds `AGENTS.md` (+ optional `HANDOVER.md`); don't filesystem-search. Resume mid-task or follow the `AGENTS.md` Session Start. **CRITICAL:** arm the watcher as a background task via `run_command` with a small `WaitMsBeforeAsync` (e.g. `1000`) — **do NOT pass `background=true`**, which is not in agy's tool schema. Copy the exact invocation from `AGENTS.md` (the `--agy` watcher force-flushes stdout and exits cleanly when someone is `WAITING ON` you). Re-arm on exit. Includes an identity reminder: you are the slug your replies are prefixed with, not generic Antigravity — re-read `AGENTS.md` if you lose track. No placeholders (it references the agent only abstractly). Backtick-free.

#### `runtimes/codex.md` — Codex Session Start

The longest, most prescriptive runtime snippet (codex has the largest failure surface). The watcher runs in a **separate sidecar tmux pane** named `{{AGENT}}-bridge`, spawned alongside the CLI pane by `giga launch`; the bridge runs `giga watch --as {{AGENT}} --codex` and drops JSON envelopes (`kind:"brief"`) into `$CODEX_CHANNEL_DIR/inbox/`. **The agent arms nothing itself.** To respond it shells out to `giga post --as {{AGENT}}`; subject prefix convention `[<slug> YYYY-MM-DD HH:MM TZ]`. Three sections:
- `## Session Start` — the bridge/envelope model above.
- `## Pane-only output ... MUST be posted to channel` — the **dominant codex comms break**: built-in slash commands (`/review`, `/diff`, `/explain`) produce **pane-only** output and do **not** auto-trigger a `giga post`, so peers wait forever. Rule: treat every pane-producing command as two halves — (1) run it, (2) `giga post` the result. Repeats "Re-read this section" to drive the discipline.
- `## Bridge-pane health` — if envelopes stop, the operator verifies with `tmux list-windows -t giga-<swarm>` and restarts the `{{AGENT}}-bridge` pane; notes that codex's "busy with another turn" error is natural backpressure (envelopes queue + retry in the bridge).

Begins with its own `## Session Start` heading. Placeholder `{{AGENT}}` appears in the bridge command, `post --as`, and the bridge-pane name.

#### `runtimes/codex-intro.md` — Codex launch prompt

Single paragraph. cwd holds `AGENTS.md`/`HANDOVER.md` (don't search). Resume mid-task or follow `AGENTS.md`. Clarifies the codex model: the watcher runs in a separate sidecar pane named `your-slug-bridge` spawned alongside the CLI, so the agent does **not** arm anything; inbound events arrive as envelopes (`kind:brief`) from `$CODEX_CHANNEL_DIR/inbox/`, and replies go via `giga post`. No placeholders. Backtick-free (uses `$CODEX_CHANNEL_DIR` without code spans).

### `ui/dashboard.html`

**Purpose.** The entire `giga ui` web dashboard in one self-contained file: HTML + inline CSS + vanilla JS, **no build step, no framework, no Node**. It is a hash-routed SPA over the axum JSON + WebSocket API. Views: a home grid of registered swarms (archive toggle, machine-level upgrade), a per-swarm view (sidebar of agents + channels with live status dots, a toolbar of validate/launch/kill/add-agent/add-channel, and a unified recent-activity timeline), an agent detail view (metadata + live tmux pane log polled every 2s), and a per-channel live tail (WebSocket snapshot + append) with an inline post composer.

**Key items.**
- A single `__VERSION__` token in the header subtitle (`dashboard.html:184`), substituted **at request time** — not compile time.
- JS helpers: `$` (`dashboard.html:203`), `el(tag, attrs, ...children)` DOM builder (`:204`), `fmtRel(iso)` relative time (`:220`), `parseHash()` (`:237`), `navTo(route)` (`:254`), `api(path)` fetch-JSON (`:263`), `closeWs()` (`:270`), `truncate(s, n)` (`:652`).
- View renderers: `renderHome` (`:276`), `renderSwarm` (`:429`), `renderSwarmOverview` (`:488`), `loadTimeline` (`:621`), `patchSidebarAgentDots` (`:662`), `renderAgent` (`:679`), `renderAddChannelForm` (`:738`), `renderChannel` (`:806`), `renderPost` (`:855`), `renderComposer` (`:870`), `renderAddAgentForm` (`:934`), `updateProcSummary` (`:1059`), `route` (`:1070`).
- Hash routes (`dashboard.html:232-235`): `#/` (home), `#/swarm/<name>`, `#/swarm/<name>/agent/<slug>`, `#/swarm/<name>/channel/<file>`.
- API/WS endpoints it calls: `/api/swarms`, `/api/swarms/{name}`, `/api/swarms/{name}/timeline?n=100`, `/api/swarms/{name}/archive`, `/api/swarms/{name}/validate|launch|kill`, `/api/swarms/{name}/agents` (add-agent), `/api/swarms/{name}/channels` (add-channel), `/api/swarms/{name}/channels/{file}` (post), `/api/swarms/{swarm}/agents/{agent}/log`, `/api/upgrade`, `/api/processes`, `/api/health`, `/assets/giga-icon.png`, and `ws://.../ws/channels/{swarm}/{file}`.

**Control flow.** `src/ui/server.rs:90` binds `const DASHBOARD_HTML = include_str!("../../templates/ui/dashboard.html")`. `index()` (`server.rs:101-106`) does `DASHBOARD_HTML.replace("__VERSION__", VERSION)` (where `VERSION = env!("CARGO_PKG_VERSION")`) and returns `Html<String>`. `build_router()` (`server.rs:36-79`) maps every fetch/WS path the JS uses to a handler in `api::`/`ws::` — the route literals match the JS strings exactly. Client lifecycle: `DOMContentLoaded` → `route()` dispatches on `parseHash()`; `hashchange` re-routes; `renderSwarmOverview` installs a 15s `setInterval` that **only** patches sidebar status dots (`patchSidebarAgentDots`) to avoid clobbering scroll position or open forms; `renderAgent` reuses the refresh timer at 2s for the pane log (frozen on `mouseenter`); `renderChannel` opens a WebSocket and dispatches on `msg.type` (`snapshot`/`append`/`error`).

**Gotchas / invariants.**
- The in-file comments are an effective changelog: v0.6.47 agent view (`:676`), v0.6.48 surgical dot refresh (`:513`, `:657`), v0.6.49/v0.6.50 archive filtering of both sidebar and grid (`:291`), v0.6.40 composer (`:866`), v0.6.43 add-agent form (`:930`), v0.6.54 host now mandatory in the add-agent form (`:947`). The design pivot from a planned Svelte SPA to no-build vanilla JS is recorded in `src/ui/server.rs:85-90`.
- **Security:** the server is localhost-only with **no auth** — `CLAUDE_OPERATOR.md` (line 26) warns never to `--bind 0.0.0.0` on an untrusted network.
- The add-agent form's host dropdown is populated from `detail.hosts` (legacy fallback: the set of agent hosts); **host is required** (no "(none)" option) as of v0.6.54. The launch form auto-suggests a 10s stagger when `agents.length >= 10`.
- The icon at `/assets/giga-icon.png` is a separate `include_bytes!` const (`ICON_PNG`, `server.rs:95`) served by `serve_icon`; it is **not** part of this HTML file.
- The `__VERSION__` substitution is request-time, so a rebuilt binary always shows the right version without re-embedding the HTML.

## Data & control flow

How the pieces interact, inside this folder and across the codebase:

- **`add-agent` path:** `render_agent_stub` (`src/add_agent.rs:535`) composes `AGENT_STUB` (`agent.md`) by splicing in the `WATCHER` (`partials/watcher.md`) and `CONVENTION` (`partials/convention.md`) partials, then substituting `{{PEERS}}`/`{{ROLE}}`/`{{AGENT}}` last so placeholders inside the injected partials are also resolved. The result is written to `agents/<slug>.md`. This path always produces a **Claude-flavored** stub regardless of the target runtime.
- **`init` path:** `render_agent_claudemd` in `src/init.rs` does **not** touch `agent.md`/`partials/watcher.md`. For a template-based agent it reads the agent's `claudemd_template`, computes the runtime snippet (`Runtime::session_start_snippet().replace("{{AGENT}}", ...)`), and injects it via `inject_session_start`, then `prepend_header` (`init.rs:384-389`). For a no-template agent it builds a minimal `AGENTS.md` inline (`init.rs:392-450`) using the runtime snippet plus a literal `## Convention` heading + trimmed `CONVENTION` body. `init` is therefore the **runtime-aware** generator; `add-agent` is not.
- **`launch` path:** `Runtime::launch_intro_prompt()` returns the matching `runtimes/<rt>-intro.md`; `src/launch.rs` single-quotes it onto the `wt.exe → wsl.exe → bash` command line (hence the no-backtick invariant). For Codex (`needs_bridge_pane()` true) `launch` also spawns the `<slug>-bridge` pane that runs the `--codex` watcher referenced by `runtimes/codex.md`.
- **`claude-operator` path:** `src/claude_operator.rs` emits `CLAUDE_OPERATOR.md` either as a `claude --append-system-prompt` suffix (TTY) or to stdout (piped/captured by an agent's Bash tool).
- **`ui` path:** `src/ui/server.rs` serves `ui/dashboard.html` with `__VERSION__` substituted per request; the embedded JS drives the swarm by calling the JSON + WebSocket handlers in `src/ui/api.rs` and `src/ui/ws.rs` — which themselves shell out to the same `giga` subcommands (validate/launch/kill/post/add-agent/add-channel/upgrade) an operator would run.

Net effect: a Claude agent's Session Start text differs slightly between the two scaffolding paths — `add-agent` bakes the verbose `partials/watcher.md` block, `init` uses the terser `runtimes/claude.md`. Both close on the single shared `CONVENTION`.

## Cross-references

Source consumers (relative to repo root):
- [`../src/templates.rs`](../src/templates.rs) — `AGENT_STUB` / `WATCHER` / `CONVENTION` consts.
- [`../src/runtime.rs`](../src/runtime.rs) — `Runtime` enum: `session_start_snippet()`, `launch_intro_prompt()`, `watcher_invocation()`, `needs_bridge_pane()`; owns the `runtimes/*.md` includes and the no-backtick test.
- [`../src/claude_operator.rs`](../src/claude_operator.rs) — `DOC` const + TTY-vs-pipe `run()` for `CLAUDE_OPERATOR.md`.
- [`../src/add_agent.rs`](../src/add_agent.rs) — `render_agent_stub`: substitutes `agent.md` + partials placeholders.
- [`../src/init.rs`](../src/init.rs) — `render_agent_claudemd`, `inject_session_start`, `prepend_header`: assembles `AGENTS.md` from runtime snippet + `CONVENTION`.
- [`../src/launch.rs`](../src/launch.rs) — single-quotes the launch intro onto the shell hop; bridge-pane / daemon-pane spawn decisions.
- [`../src/ui/server.rs`](../src/ui/server.rs) — `DASHBOARD_HTML` + `ICON_PNG` consts, `build_router` routes, request-time `__VERSION__` substitution.
- [`../src/ui/api.rs`](../src/ui/api.rs) and [`../src/ui/ws.rs`](../src/ui/ws.rs) — the JSON + WebSocket handlers the dashboard JS targets.
- [`../src/main.rs`](../src/main.rs) — `ClaudeOperator` subcommand wiring (`main.rs:335`).
- [`../assets/giga-icon.png`](../assets/giga-icon.png) — `include_bytes!` brand icon referenced by `dashboard.html`.

Design / docs:
- `UI_DESIGN.md` — cited in the comment at `src/ui/server.rs:86` as the doc that originally scoped a Svelte SPA before the pivot to no-build vanilla JS. Note: this doc is referenced by name but is **not present** in the current tree (`docs/`/`design/`).
- [`../design/SWARM_BOSS_DESIGN.md`](../design/SWARM_BOSS_DESIGN.md) — swarm_boss arming of sync + merger Monitors (the swarm_boss section of `CLAUDE_OPERATOR.md`).
- [`../design/README.md`](../design/README.md) — index of the design docs (broadcast fanout, remote/dual-write transport, stale-waits, teleport, etc.).
- [`../README.md`](../README.md) — overview + full subcommand table.
- [`../docs/QUICKSTART.md`](../docs/QUICKSTART.md), [`../docs/COMMAND_REFERENCE.md`](../docs/COMMAND_REFERENCE.md), [`../docs/MANUAL_SETUP.md`](../docs/MANUAL_SETUP.md), [`../docs/REMOTE_QUICKSTART.md`](../docs/REMOTE_QUICKSTART.md) — task-specific guides. (The operator doc's "Where to read more" lists these by bare filename; they live under `docs/`.)
