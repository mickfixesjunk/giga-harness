# `src/ui/` — the `giga ui` dashboard server

A localhost-default [axum](https://docs.rs/axum) HTTP + WebSocket server that gives the operator a browser dashboard over every swarm registered on the machine (`~/.giga/swarms.toml`). It is the single async/Tokio island in an otherwise synchronous CLI.

## Role in the system

`giga` is a synchronous, file-based multi-agent coordination harness — every other subcommand (`post`, `launch`, `watch`, `add-agent`, …) is plain blocking I/O. The `giga ui` subcommand is the one exception: `ui::run` spins up a **scoped** multi-thread Tokio runtime (deliberately *not* `#[tokio::main]`, so async stays contained to this command) and blocks on an axum server. The server is **stateless per request** — it reloads the registry plus each swarm's `giga-harness.toml` on every call — and **never re-implements** mutation logic: posting, validating, launching, killing, adding agents/channels, and upgrading all shell out to the same `giga` binary, `tmux`, or `ps`. The only persistent in-memory state is `AppState.tailers`, a map of `(swarm, channel-file)` to a `broadcast::Sender<Post>` fed by a 500 ms file-polling task that fans newly-appended posts to all WebSocket subscribers.

## File index

| File | Lines (approx) | Purpose |
| --- | --- | --- |
| [`mod.rs`](./mod.rs) | 62 | Subcommand entry point: `Args`, `run`, PID-file path resolution, submodule declarations. |
| [`server.rs`](./server.rs) | 201 | axum `Router` (all REST + WS routes), `serve` with Ctrl-C shutdown, embedded dashboard HTML + brand icon, `/`/`/api/health` handlers. |
| [`api.rs`](./api.rs) | 980 | All REST handlers + their DTOs. Read endpoints serialize registry/config/process state; mutation endpoints validate then shell out via `run_giga` / `tmux`. |
| [`ws.rs`](./ws.rs) | 367 | WebSocket live-tail endpoint: snapshot-then-append wire protocol, per-channel tailer lifecycle. |
| [`channel.rs`](./channel.rs) | 248 | `===`-delimited channel-post parser (`Post`, `parse`); mirrors `watch::is_header_line`. |
| [`process.rs`](./process.rs) | 298 | Read-only process introspection via `tmux list-sessions/list-windows` and `ps -eo pid,args`. |
| [`pid.rs`](./pid.rs) | 181 | Single-instance enforcement via `~/.giga/ui.pid` (RAII `Guard`) + an `is_alive` probe reused by `giga launch --ui`. |
| [`state.rs`](./state.rs) | 25 | `AppState`: the cheaply-clonable Arc-backed tailers map injected into every handler. |

## Files

### `mod.rs` — entry point & wiring

**Purpose.** Declares the seven submodules (`api`, `channel`, `pid`, `process`, `server`, `state`, `ws`), defines the CLI args, and owns process lifecycle for `giga ui`.

**Key public items.**
- `pub struct Args { bind: String, port: u16 }` — populated from `Command::Ui { bind, port }` in `main.rs`.
- `pub fn run(args: Args) -> Result<()>` — the whole subcommand. Acquires the PID lock, prints a startup banner, builds the Tokio runtime, and `block_on`s the server.

**Control flow.** `run` (mod.rs:32) calls `pid_file_path()?` then `pid::acquire(&pid_path)?` *first* — if a live server already holds the lock the call bails before binding, avoiding `EADDRINUSE`. It then builds a multi-thread runtime via `tokio::runtime::Builder::new_multi_thread().enable_all().build()` (mod.rs:43-46, mapping the build error through `anyhow` by hand) and `rt.block_on(server::serve(args.bind, args.port))` (mod.rs:48). On normal return — or Ctrl-C unwind — the held `_guard` drops and unlinks the PID file.

`fn pid_file_path() -> Result<PathBuf>` (mod.rs:57) returns `crate::cursor::giga_home().join("ui.pid")`, erroring out if neither `$HOME` nor `%USERPROFILE%` is set.

**Gotchas.** The module doc comment still describes the v0.6.31 Phase A "hello world" scope and a future Svelte SPA — both stale. Treat the route list in `server.rs` as ground truth for the actual surface.

### `server.rs` — router, serve loop, embedded assets

**Purpose.** Defines the full route table mapping URLs to handlers in `api.rs`/`ws.rs`, the async `serve` entrypoint with graceful shutdown, and the two embedded static assets.

**Key public/internal items.**
- `pub async fn serve(bind, port) -> Result<()>` — binds a `tokio::net::TcpListener` on `{bind}:{port}` and runs `axum::serve(listener, app).with_graceful_shutdown(shutdown_signal())`.
- `async fn shutdown_signal()` — awaits `tokio::signal::ctrl_c()`, prints, then returns to let axum drain in-flight requests.
- `fn build_router() -> Router` — the route table (server.rs:36-80), finished with `.with_state(AppState::new())`.
- `async fn index() -> Html<String>` — serves `DASHBOARD_HTML` with the literal `__VERSION__` placeholder replaced by `CARGO_PKG_VERSION` **at request time**.
- `async fn serve_icon()` — returns `ICON_PNG` with `Content-Type: image/png`.
- `async fn health() -> Json<Health>` (struct `Health { status, version }`) — `GET /api/health`.
- `const DASHBOARD_HTML = include_str!("../../templates/ui/dashboard.html")`, `const ICON_PNG = include_bytes!("../../assets/giga-icon.png")`, `const VERSION = env!("CARGO_PKG_VERSION")`.

**Route table (`build_router`, server.rs:36-80).**

| Method | Path | Handler |
| --- | --- | --- |
| `GET` | `/` | `index` (dashboard HTML) |
| `GET` | `/api/health` | `health` |
| `GET` | `/api/swarms` | `api::list_swarms` |
| `GET` | `/api/swarms/{name}` | `api::get_swarm` |
| `GET` | `/api/swarms/{name}/channels/{file}` | `api::get_channel_tail` |
| `POST` | `/api/swarms/{name}/channels/{file}` | `api::post_to_channel` |
| `GET` | `/api/swarms/{name}/timeline` | `api::get_swarm_timeline` |
| `POST` | `/api/swarms/{name}/archive` | `api::set_swarm_archived` |
| `POST` | `/api/swarms/{name}/validate` | `api::validate_swarm` |
| `POST` | `/api/swarms/{name}/launch` | `api::launch_swarm` |
| `POST` | `/api/swarms/{name}/kill` | `api::kill_swarm` |
| `POST` | `/api/swarms/{name}/agents` | `api::add_agent` |
| `POST` | `/api/swarms/{name}/channels` | `api::add_channel` |
| `GET` | `/api/swarms/{swarm}/agents/{agent}/log` | `api::get_agent_log` |
| `GET` | `/api/processes` | `api::list_processes` |
| `POST` | `/api/upgrade` | `api::run_upgrade` |
| `GET` | `/assets/giga-icon.png` | `serve_icon` |
| `GET` | `/ws/channels/{swarm}/{file}` | `ws::ws_channel` |

**Gotchas.** The frontend is no-build vanilla JS embedded via `include_str!` (server.rs:82-89 doc) — pivoted away from the originally-scoped Svelte SPA so end users don't need Node to build `giga`; the binary ships as one artifact (HTML + PNG embedded). Because version substitution happens at request time, the `DASHBOARD_HTML` constant *retains* the literal `__VERSION__`; tests assert both the present-in-template placeholder and its absence in the served page. Tests at the bottom also assert dashboard markers, health JSON, and 404 on an unknown path.

### `api.rs` — REST handlers + DTOs

**Purpose.** Every REST handler and its request/response DTO. Stateless: each handler freshly reloads `registry::load()` and per-swarm `Config::load(entry.config)`. Reads serialize swarm/channel/process state; mutations validate then shell out — never re-implementing post/validate/launch/add logic.

**Read handlers.**
- `pub async fn list_swarms() -> Json<Vec<SwarmSummary>>` — maps every registry entry through `summarize_swarm`; **swallows registry load errors as an empty list**.
- `pub async fn get_swarm(Path(name)) -> Result<Json<SwarmDetail>, StatusCode>` — 404 if not registered, 500 if config won't load; delegates to `detail_from`.
- `pub async fn get_channel_tail(Path((name, file)), Query(TailQuery))` — **path-traversal-safe**: 404 unless `file` is declared in `cfg.channels`; reads `inbox_dir_for(cfg, channel_meta).join(file)`; `n` defaults 50, capped 500; returns the last `n` posts plus a `total`.
- `pub async fn get_swarm_timeline(Path(name), Query(TailQuery))` — aggregates posts across **all** channels, sorts newest-first by ISO timestamp (lexical), truncates to `n` (default 100, cap 500), reports `total_scanned`.
- `pub async fn list_processes() -> Json<process::ProcessSnapshot>`.
- `pub async fn get_agent_log(Path((swarm, agent)), Query(LogQuery))` — captures the agent's tmux pane via `tmux capture-pane -p -S -<lines>`.

**Mutation / exec handlers.**
- `pub async fn post_to_channel(Path((name, file)), Json(PostBody))` — pre-checks the channel is declared, then appends via `crate::post::run`. Maps `"is not a participant"` / `"WAITING ON target"` errors to **400**, everything else to **500**.
- `pub async fn set_swarm_archived(Path(name), Json(ArchiveBody))` — calls `registry::set_archived`; `"is not registered"` → 404, else 500.
- `pub async fn validate_swarm` / `launch_swarm` / `kill_swarm` / `add_agent` / `add_channel` / `run_upgrade`.

**DTOs.** `SwarmSummary`, `SwarmDetail`, `AgentDto`, `AgentProcessStatus`, `ChannelDto`, `TailQuery`, `TimelinePost`, `Timeline`, `ChannelTail`, `ArchiveBody`/`ArchiveResult`, `PostBody`/`PostResponse`/`PostError`, `ExecResult`, `UpgradeQuery`, `LaunchQuery`, `LogQuery`/`LogSnapshot`, `AddAgentBody`, `AddChannelBody`.

**Helpers.** `summarize_swarm`, `detail_from`, `agent_dto`, `agent_has_tmux_window`, `channel_dto`, `last_activity`, `newest_md_mtime`, `inbox_dir_for`, `not_found`, `internal`, and the shell-out shim:

```rust
fn run_giga(argv: &[&str]) -> Result<ExecResult, std::io::Error>
```

It runs `std::env::current_exe()` (falling back to `"giga"`) with **captured** stdout/stderr (never inherited), so the child sees the same binary/behavior as the running server.

**Control flow & encoded quirks.**
- Read path: handler → `registry::load()` → find entry by name → `Config::load(entry.config)` → serialize. `get_channel_tail`, `get_swarm_timeline`, and `inbox_dir_for` pick the inbox by `channel.side` (`"windows"` → `paths.windows_inbox`, else `paths.wsl_inbox`) and parse with `post_parser::parse`.
- `detail_from` (api.rs:807) takes one `process::snapshot()` and cross-references the `giga-<swarm>` session's tmux window names plus watcher agent slugs to fill each `AgentDto.process_status`: `tmux_alive` via `agent_has_tmux_window` (matches `slug`, `slug-bridge`, or `slug-cli`), `watcher_alive` by slug membership in the watcher list.
- `giga validate` takes **CONFIG positionally**, not `--config` (api.rs:438-442) — unlike `post`/`launch`.
- `kill_swarm` verifies registry membership *before* `tmux kill-session -t giga-<name>` so a typo can't hit a foreign session.
- `launch_swarm` defaults to `--skip-init` + `--terminal tmux`; `?init=true` re-renders AGENTS.md; `?stagger=N` (>0) adds `--stagger-per-agent-seconds N`.
- `add_agent` rejects non-`[a-zA-Z0-9_-]` slugs (400) before spawning.
- `add_channel` enforces **exactly 2 participants** (400 otherwise).
- `run_upgrade` always passes `--bare` (+ optional `--dry-run`).
- `get_agent_log` tries `<slug>` then `<slug>-cli` and returns `captured: false` with empty content rather than 404 when no window matches.
- `last_activity` / `newest_md_mtime` scan only the **WSL** inbox's `*.md` mtimes (ignores the windows inbox), formatting the newest as RFC3339 via `chrono`.
- `summarize_swarm` never panics on a bad config — it returns `load_error` populated with zero counts (tested).

**Gotchas — SECURITY.** There is **no auth in v1** (api.rs:338-341). Any client that can reach the bind address can post to channels, launch/kill swarms, add agents, and trigger `giga upgrade`. This is safe only because the default bind is localhost; operators who `--bind 0.0.0.0` own the tailnet trust model. Path traversal is blocked for channel reads/posts because only files listed in `[[channels]]` are reachable. Synchronous `std::fs` / `std::process::Command` runs inside the async handlers deliberately (a few syscalls; not worth `spawn_blocking`). The timeline newest-first sort relies on RFC3339/8601-Z timestamps being lexically orderable.

### `ws.rs` — live channel tail (WebSocket)

**Purpose.** The `GET /ws/channels/{swarm}/{file}` endpoint. On connect it sends a one-time JSON snapshot of the last 50 posts, then forwards each newly-appended post. Owns the tailer lifecycle.

**Key items.**
- `pub async fn ws_channel(Path((swarm, file)), WebSocketUpgrade, State(AppState)) -> impl IntoResponse` — upgrades and hands off to `handle_socket`.
- `async fn handle_socket(socket, swarm, file, state)` — splits the socket, resolves the path, sends the snapshot, subscribes to the tailer, runs the forward loop.
- `enum WireEvent<'a> { Snapshot { posts }, Append { post }, Error { message } }` — `#[serde(tag = "type", rename_all = "lowercase")]`; the JSON wire protocol (`{"type":"snapshot"|"append"|"error", …}`).
- `async fn ensure_tailer(state, swarm, file, path) -> broadcast::Receiver<Post>` — read-lock fast path then write-lock double-check (race-safe), inserts a `Sender` and spawns `run_tailer` on first creation.
- `async fn run_tailer(path, tx: broadcast::Sender<Post>)` — 500 ms interval poll; baseline `last_count` = post count at spawn; broadcasts only `posts[last_count..]`; resets the baseline on truncation/rewrite (no re-broadcast of history).
- `fn resolve_channel_path(swarm, file) -> Result<PathBuf, String>` — registry + config lookup, `"channel not in swarm config"` guard, inbox-side selection (mirrors `api.rs`).
- Helpers `send_event` / `reader_done`; consts `SNAPSHOT_POSTS = 50`, `BROADCAST_CAPACITY = 256`, `POLL_INTERVAL = 500ms`.

**Control flow.** `ws_channel` → `on_upgrade` → `handle_socket` (ws.rs:62): split into sender/receiver; `resolve_channel_path` (errors send a `WireEvent::Error` then close); read + parse the file and send a `Snapshot` of the last 50; `ensure_tailer` subscribes (or spawns `run_tailer`). A spawned reader task drains incoming frames (keeps keepalive pings moving and detects client-initiated close). The main loop is a `tokio::select!` between `reader_done` (a 250 ms `JoinHandle::is_finished` poll loop) and `rx.recv()`: on `Ok(post)` send `Append`; on `Lagged` send the `"subscriber lagged — reconnect"` error and break; on `Closed` break. Cleanup: `sender.close()` + `reader.abort()`. In `run_tailer`, `tx.send` is a cheap no-op (`Err` ignored) when there are zero receivers, but the internal `last_count` advances regardless.

**Gotchas.** Tailers run **forever** once spawned — no idle cleanup in v1 (the doc notes a v2 "drop+respawn if idle >5 min"). There is exactly one tailer per `(swarm, file)` key, deduped across subscribers *and* across distinct connecting clients (tested `ensure_tailer_dedups_per_channel_key`). A slow consumer that overflows the 256-deep ring gets a `Lagged` error and the server closes the socket, expecting the client to reconnect and re-snapshot. `reader_done` polls rather than borrowing the `JoinHandle` into `select!` (it can't be re-borrowed across iterations). The snapshot count (50) is independent of the REST tail default (also 50) and the timeline default (100). Truncating/rewriting the channel file resets the baseline without replaying history (tested `run_tailer_resets_after_file_truncated`).

### `channel.rs` — channel-post parser

**Purpose.** Converts an append-only `===`-delimited channel `.md` log into `Vec<Post>`. Shared by the REST endpoints and the WebSocket tailer/snapshot.

**Key items.**
- `pub struct Post { sender, subject, timestamp_iso, body }` — derives `Debug, Clone, Serialize, PartialEq, Eq`; the canonical on-the-wire post shape.
- `pub fn parse(content: &str) -> Vec<Post>` — scans for `===` / header / `===` triples; body runs until the next standalone `===` or EOF; **oldest-first** order.
- `fn is_header_line(line) -> bool` — mirrors `watch::is_header_line`: starts with `[`, contains `"] "`, is not `[<…`, and the last 20 bytes match the UTC ISO tail (`Z` at byte 19, `-` at 4/7, `T` at 10, `:` at 13/16).
- `fn parse_header(line) -> (sender, subject, timestamp)` — `sender` = first `[…]` group, `timestamp` = last 20 bytes, `subject` = the middle, trimmed of a trailing em-dash.

**Control flow.** `parse` (channel.rs:53) walks lines: at a standalone `===` it requires a header at `i+1` and a closing `===` at `i+2`; the body is `lines[i+3..next ===]` joined and trimmed; `i` advances to `body_end` so the shared `===` between consecutive posts is reused as the next opener. Non-header or malformed triples are skipped (`i += 1`).

**Gotchas.** Header detection is **byte-slice based** to avoid UTF-8 boundary panics from em-dashes near the 20-byte timestamp tail (regression test `is_header_line_handles_multibyte_tail_without_panic`). Convention placeholder lines like `[<sender>] …` are rejected by the `[<` guard so AGENTS.md preamble isn't parsed as a post. `timestamp_iso` is empty for malformed headers (shouldn't occur for giga-written files). The header rule is **intentionally duplicated** from `watch.rs` rather than imported (channel.rs:96-98) — if watch's contract changes, this must be mirrored. The parser re-runs on every poll tick and every REST request (no caching/incremental parsing).

### `process.rs` — process introspection

**Purpose.** Read-only discovery backing `GET /api/processes` and the per-agent alive flags in `SwarmDetail`. Shells out to `tmux` and `ps`.

**Key items.**
- `pub fn snapshot() -> ProcessSnapshot` — combines `tmux_sessions()` and `watcher_processes()`, each `unwrap_or_default` so a missing `tmux`/`ps` degrades to empty.
- `pub fn tmux_sessions() -> Result<Vec<TmuxSession>, io::Error>` — `tmux list-sessions -F #{session_name}`, then per session `tmux list-windows -F #{window_name}\t#{pane_pid}`.
- `pub fn watcher_processes() -> Result<Vec<WatcherProcess>, io::Error>` — `ps -eo pid=,args=` parsed by `parse_ps_output`.
- Types `ProcessSnapshot { tmux, watchers }`, `TmuxSession { name, windows }`, `TmuxWindow { name, pane_pid: Option<u32> }`, `WatcherProcess { pid, agent, runtime }`.
- Internal parsers `parse_tmux_windows`, `parse_ps_output`, `extract_as_slug`, `run`.

**Control flow.** `snapshot` (process.rs:52) gathers tmux sessions/windows and watcher rows. `parse_ps_output` keeps rows whose args contain `"giga watch"`, parses the pid, extracts the slug via `extract_as_slug` (the token after `--as ` restricted to `[a-zA-Z0-9_-]`), and classifies runtime by a **leading-space** `" --codex"` / `" --agy"` (default `"claude"`). `api::detail_from` consumes the snapshot to set `AgentProcessStatus`.

**Gotchas.** `extract_as_slug` deliberately stops at the first non-slug char so bash/eval-wrapped invocations like `eval 'giga watch --as superdeduper' < /dev/null` yield `superdeduper`, not `superdeduper'` (regression test `extract_as_slug_strips_trailing_shell_metacharacters`). `run` converts a non-zero exit to `io::Error` so callers can `unwrap_or_default` to empty; it uses `argv[0]` as the program, so the first element must be the binary name. The leading-space match on runtime flags avoids false hits inside slugs.

### `pid.rs` — single-instance lock

**Purpose.** Enforces a single `giga ui` via `~/.giga/ui.pid`, and exposes a liveness probe reused by `giga launch --ui`.

**Key items.**
- `pub struct Guard { path }` with `impl Drop` — best-effort removes the PID file on drop.
- `pub fn acquire(path: &Path) -> Result<Guard>` — `mkdir -p` the parent; if the file holds a **live** PID, bail loudly with that PID; if dead/stale, warn and overwrite; writes `std::process::id()`.
- `pub fn is_alive(path: &Path) -> bool` — reads + parses the PID and returns `process_alive`; missing/malformed/dead ⇒ `false`. Called from `launch.rs`.
- `fn process_alive(pid)` — `#[cfg(unix)]` uses `kill(pid, 0)`; `#[cfg(not(unix))]` always returns `true` (Phase A Windows punt).
- `extern "C" fn kill` + `unsafe fn libc_kill` — a raw POSIX existence probe (no `libc` crate dependency).

**Control flow.** `mod.rs::run` calls `acquire` first, so a live prior server makes the call bail before binding. The `Guard` is held for the server lifetime; normal exit / Ctrl-C unwinds and Drop unlinks the file. Separately, `launch.rs` calls `crate::ui::pid::is_alive(home.join("ui.pid"))` so `giga launch --ui` skips scheduling a `giga ui` pane when a server is already running, and picks back up after a crash (stale PID).

**Gotchas.** Windows liveness is stubbed to always-`true` (single-user workstation assumption), so a stale Windows PID file reports alive and `acquire` refuses to start — the overwrite-stale test is therefore `#[cfg(unix)]`-gated. `Guard` Drop is best-effort: if the file was deleted out from under the process, the next `acquire` treats it as stale. The `kill(2)` probe checks existence **and** signal permission, so a PID owned by another user can read as not-alive.

### `state.rs` — shared server state

**Purpose.** The only persistent in-process state.

**Key items.**
- `pub struct AppState { tailers: Arc<RwLock<HashMap<(String, String), broadcast::Sender<Post>>>> }` — derives `Clone, Default`.
- `pub fn new() -> Self` — `Self::default()`.

**Control flow.** `server::build_router().with_state(AppState::new())` injects one shared instance into every handler. `ws::ensure_tailer` reads/writes `AppState.tailers` keyed by `(swarm, channel-file)`, inserting a `Sender` and spawning a tailer on first subscription; later subscribers clone-subscribe the existing sender.

**Gotchas.** Every field is `Arc`, so axum hands each handler a cheap `Clone` sharing the same underlying map and tailer tasks. It uses `tokio::sync::RwLock` (async), not `std::sync`, because lock holders are awaited across `.await` points in `ensure_tailer`. The map **only grows** — there is no eviction of tailers in v1, matching `run_tailer`'s forever lifetime.

## Data & control flow

**Inside the folder.** `mod.rs::run` → `pid::acquire` → Tokio runtime → `server::serve` → `build_router().with_state(AppState::new())`. Every read handler in `api.rs` is independent and stateless: `registry::load()` → `Config::load(entry.config)` → serialize, using `channel::parse` for post extraction and `process::snapshot()` for liveness. The single shared object is `AppState.tailers`: `ws::ensure_tailer` lazily creates one `broadcast` channel + `run_tailer` polling task per `(swarm, file)`; all WS clients of that channel share it.

**Across folders.**
- `src/main.rs` dispatches `Command::Ui { bind, port } => ui::run(ui::Args { bind, port })` (main.rs:828) — the sole entry into this module.
- `src/registry.rs` — `registry::load()` / `Entry { name, config, code_roots, archived }` / `set_archived` back the swarm list and archive toggle (`~/.giga/swarms.toml`).
- `src/config.rs` — `Config::load`, plus fields `agents`, `channels`, `paths.{wsl_inbox, windows_inbox}`, `project.{description, runtime, launch_model}`, `this_host`, `hosts`, and `Config::agent_runtime(a)`.
- `src/post.rs` — `crate::post::run` is the canonical channel-append invoked by `post_to_channel`; its participant / WAITING-ON validation errors surface as 400s.
- `src/cursor.rs` — `giga_home()` resolves `~/.giga` for the PID path here and the `is_alive` check in `launch.rs`.
- `src/launch.rs` — calls `crate::ui::pid::is_alive` to decide whether `giga launch --ui` schedules a `giga ui` daemon pane (the cross-command integration point).
- `src/watch.rs` — `watch::is_header_line` is the contract `channel::is_header_line` mirrors (duplicated, not imported); `process.rs` discovers the `giga watch --as <slug>` processes that `watch.rs` spawns.
- **Subprocesses** — `api.rs`/`process.rs` shell out to the running `giga` binary (`validate`/`launch`/`kill`/`add-agent`/`add-channel`/`upgrade` via `run_giga` → `std::env::current_exe`), plus `tmux` (`kill-session`, `capture-pane`, `list-sessions`/`list-windows`) and `ps`.
- **Embedded assets** — [`../../templates/ui/dashboard.html`](../../templates/ui/dashboard.html) and [`../../assets/giga-icon.png`](../../assets/giga-icon.png), the client that consumes this whole API + WS.
- **External crates** — `axum` (`Router`, `extract::{Path, Query, State, ws}`, `Json`, `response::{Html, IntoResponse}`), `tokio` (`runtime`, `net::TcpListener`, `signal::ctrl_c`, `sync::{broadcast, RwLock}`, `time::interval`), `futures_util` (sink/stream split for WS), `serde`/`serde_json`, `chrono` (RFC3339 mtime), `tempfile` (tests).

## Cross-references

- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) — system-wide architecture hub; see its "Subsystems → UI dashboard" section.
- [`../README.md`](../README.md) — the `src/` module map this server lives under.
- [`../../README.md`](../../README.md) — top-level project overview.
- [`../main.rs`](../main.rs) — CLI dispatch (`Command::Ui`).
- [`../registry.rs`](../registry.rs), [`../config.rs`](../config.rs), [`../post.rs`](../post.rs), [`../launch.rs`](../launch.rs), [`../watch.rs`](../watch.rs), [`../cursor.rs`](../cursor.rs) — collaborating modules.
- [`../../design/BROADCAST_FANOUT_DESIGN.md`](../../design/BROADCAST_FANOUT_DESIGN.md) — the broadcast/fanout machinery referenced from `post::Args::to`, relevant to how `post_to_channel` maps onto channel routing.
- [`../../docs/COMMAND_REFERENCE.md`](../../docs/COMMAND_REFERENCE.md) — operator-facing reference for the `giga` subcommands this server shells out to.

> **Note:** the module doc comments reference `workdirs/giga/UI_DESIGN.md` (the original UI design that scoped a Svelte SPA later pivoted to embedded vanilla JS). That file is not present in this checkout; the route table in `server.rs` is the authoritative API surface.
