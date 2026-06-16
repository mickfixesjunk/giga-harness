# `src/` — giga-harness core CLI

The Rust source for the `giga` binary (crate `giga-harness`). One module per subcommand/feature: config schema and validation, on-disk scaffolding, terminal spawning, the file-based messaging substrate (post/watch/merge/sweep/stale-wait), agent/channel/host management, the cross-host transport plumbing, and the operator lifecycle commands (teleport/takeover/upgrade/setup). All coordination is plain text in shared Markdown files — there is no database, no message bus, and no LLM in the loop.

## Role in the system

`giga` turns one canonical `giga-harness.toml` into a runnable swarm of parallel AI coding agents that coordinate by appending convention-formatted frames to shared Markdown "channel" files and tailing them. This folder is the whole CLI surface: [`main.rs`](./main.rs) parses args and dispatches each subcommand to its module, almost always after resolving the right config via [`registry.rs`](./registry.rs). The modules split into a *control plane* that mutates the TOML and scaffolds the filesystem (`init`, `add_agent`, `add_channel`, `add_host`, `set_swarm_boss`, `teleport`, `takeover`) and a *data plane* of long-running daemons that move messages (`watch`, `merger`, `sync`, `codex_channel`). Cross-host work is abstracted behind the `Transport` trait, whose concrete plugs live in the [`transports/`](./transports/) subfolder; the web dashboard lives in [`ui/`](./ui/).

## File index

| File | Lines (approx) | Purpose |
|------|---------------:|---------|
| [`main.rs`](./main.rs) | 1147 | Binary entrypoint; `Cli`/`Command` clap surface and subcommand dispatch. |
| [`config.rs`](./config.rs) | 2101 | TOML schema, validation, resolution — single source of truth for a swarm. |
| [`validate.rs`](./validate.rs) | 188 | `giga validate`: read-only config check + orphan-channel scan. |
| [`fs_paths.rs`](./fs_paths.rs) | 195 | Cross-platform path translation (Windows drive ↔ WSL `/mnt`). |
| [`templates.rs`](./templates.rs) | 20 | `include_str!`-baked AGENTS.md template constants. |
| [`runtime.rs`](./runtime.rs) | 387 | `Runtime` enum (Claude/Codex/Agy): launch cmd, watcher mode, snippets. |
| [`init.rs`](./init.rs) | 1151 | `giga init`: idempotent scaffolder + AGENTS.md rendering pipeline. |
| [`launch.rs`](./launch.rs) | 920 | `giga launch`: config → panes → terminal spawn; daemon-spawn matrix. |
| [`terminal.rs`](./terminal.rs) | 670 | Multiplexer detection + the four spawn backends (WT/tmux/Mac/print). |
| [`trust.rs`](./trust.rs) | 280 | Pre-seeds Claude Code per-folder trust (`~/.claude.json`). |
| [`registry.rs`](./registry.rs) | 431 | Cross-swarm registry (`~/.giga/swarms.toml`); `resolve_config`. |
| [`cursor.rs`](./cursor.rs) | 150 | Per-agent read cursors + per-slice merge cursors under `~/.giga`. |
| [`post.rs`](./post.rs) | 922 | `giga post`: append a canonical frame; exclusive-lock append primitive. |
| [`watch.rs`](./watch.rs) | 1072 | `giga watch`: per-agent inbox watcher (3 delivery modes). |
| [`merger.rs`](./merger.rs) | 632 | `giga merger`: sole-writer daemon folding peer slices into merged file. |
| [`sweep.rs`](./sweep.rs) | 152 | `giga sweep`: one-shot table of each channel's last message + WAITING ON. |
| [`stale_wait.rs`](./stale_wait.rs) | 548 | Pure no-LLM stale `WAITING ON: <me>` re-derivation. |
| [`codex_channel.rs`](./codex_channel.rs) | 285 | `giga codex-channel`: bridge inbox notifications into Codex JSON envelopes. |
| [`add_agent.rs`](./add_agent.rs) | 1316 | `giga add-agent`: scaffold a new agent + peer channels + broadcast enrollment. |
| [`add_channel.rs`](./add_channel.rs) | 286 | `giga add-channel`: add one bilateral channel between existing agents. |
| [`set_swarm_boss.rs`](./set_swarm_boss.rs) | 218 | `giga set-swarm-boss`: promote/demote the per-host supervisory role. |
| [`claude_operator.rs`](./claude_operator.rs) | 63 | `giga claude-operator`: TTY-aware operator command-surface doc/launcher. |
| [`switch.rs`](./switch.rs) | 518 | `giga switch`: swap active Claude account credentials (unix-only). |
| [`transport.rs`](./transport.rs) | 226 | `Transport` trait + `for_config` factory. |
| [`sync.rs`](./sync.rs) | 1400 | `giga sync` daemon + rsync-over-Tailscale-SSH engine + peer bootstrap. |
| [`remote.rs`](./remote.rs) | 279 | `giga remote --host`: SSH-passthrough primitive. |
| [`add_host.rs`](./add_host.rs) | 533 | `giga add-host`: register a peer + first-host local→multi-host migration. |
| [`hosts.rs`](./hosts.rs) | 393 | `giga hosts`: read-only topology/roster inspector. |
| [`setup_remote_node.rs`](./setup_remote_node.rs) | 444 | `giga setup --remote-node`: on-the-peer installer. |
| [`teleport.rs`](./teleport.rs) | 720 | `giga teleport`: move a running agent between hosts. |
| [`takeover.rs`](./takeover.rs) | 619 | `giga takeover`: flip an agent's runtime in place. |
| [`upgrade.rs`](./upgrade.rs) | 1302 | `giga upgrade`: reinstall the binary locally + on peers + rearm broadcast. |
| [`setup.rs`](./setup.rs) | 468 | `giga setup`: zero-state bootstrap via a baked-in Claude prompt. |
| [`transports/`](./transports/) | — | Concrete `Transport` plugs (local / rsync+tailscale / git). See subfolder. |
| [`ui/`](./ui/) | — | axum + websocket web dashboard (`giga ui`). See subfolder. |

## Files

### Core: config, validation, entrypoint, runtimes

#### [`main.rs`](./main.rs)
Binary entrypoint and the entire CLI surface. Defines `struct Cli` and `enum Command` (Setup, Validate, Init, Launch, Teleport, Takeover, SetSwarmBoss, Upgrade, Ui, Hosts, ClaudeOperator, Sweep, Post, AddAgent, AddHost, AddChannel, Switch, Watch, Merger, Sync, Remote, CodexChannel) and `fn main`.

- Most arms call `registry::resolve_config` first (16 call sites) so a bare `giga <cmd>` works from any cwd under a swarm. `ClaudeOperator`, `Ui`, and non-`--remote-node` `Setup` skip resolution.
- `Launch`/`Sweep` with `--host` forward via `remote::run`; `Upgrade` falls back to `upgrade::run_bare` when `resolve_config` fails (cwd not under a swarm); `Takeover` parses `runtime::Runtime`; `Watch` derives `watch::WatchMode` (`main.rs:1043-1047`).
- Gotcha: `AddAgent` passes its config path through **without** `resolve_config` (unlike `AddChannel`/`SetSwarmBoss`), so it operates on the path exactly as given. The `--as` flag uses a raw identifier to dodge clap's reserved-word handling.

#### [`config.rs`](./config.rs)
The TOML schema, validation, and resolution logic — the single source of truth for a swarm. Defines `struct Config`/`Project`/`Paths`/`Host`/`Agent`/`Channel`/`BenchProtocol`/`TransportConfig`/`GitTransportConfig`/`BroadcastConfig`/`WatchConfig`, the `enum BroadcastPrefix` (`Fyi`/`Ack`/`All`/`GigaRearm`), and the broadcast helpers `parse_broadcast_prefix`/`is_broadcast_channel`/`fanout_delay_seconds`.

- `Config::load` canonicalizes the config path (v0.3.7, for symlink siblings), reads `this_host.local.toml` (const `THIS_HOST_FILE`) or the legacy `this_host.toml` (`THIS_HOST_FILE_LEGACY`) via `load_this_host`, fills inboxes (`apply_path_defaults`, `resolve_windows_userprofile`), then runs `validate` (unique hosts; explicit agent host required in a multi-host swarm since v0.3.8; one `swarm_boss` per host; boss must be WSL). `this_host` and `source_path` are `serde(skip)`.
- Accessors used everywhere: `agent_host`, `agent_runtime`, `channel_is_local`, `channel_path`, `inbox_for_host_side`, `agent_by_name`.
- `agent_runtime` priority: agent-level → project-level → Claude default. Lines ~946–2101 are tests.

#### [`validate.rs`](./validate.rs)
`giga validate` — a read-only check with no side effects. `pub fn run` prints the swarm summary and per-channel existence, then scans inbox dirs (via `to_host_fs`) for non-enrolled, channel-looking orphan files using `fn looks_like_channel`. The actual schema validation lives in `Config::load`; this command is purely informational.

#### [`fs_paths.rs`](./fs_paths.rs)
Pure, unit-tested cross-platform path translation. `pub fn to_host_fs` (cfg-gated) maps a Windows drive path or a WSL `/mnt/<drive>` mount to the current host's form; `fn windows_to_wsl` validates byte-by-byte; `fn wsl_to_windows` strips `/mnt` and maps back. Consumed by `config::channel_path` and `validate`.

#### [`templates.rs`](./templates.rs)
Bakes AGENTS.md template text into the binary via `include_str!`. Exports `pub const AGENT_STUB`, `pub const WATCHER`, `pub const CONVENTION`. `init.rs` and `add_agent.rs` substitute placeholders (`{{AGENT}}`/`{{ROLE}}`/`{{PEERS}}`/`{{WATCHER}}`/`{{CONVENTION}}`). Edit the underlying `.md` files then recompile.

#### [`runtime.rs`](./runtime.rs)
`enum Runtime { Claude, Codex, Agy }` abstracts per-runtime launch behaviour. Methods: `as_str`, `parse`, `default_launch_cmd`, `watcher_invocation`, `needs_bridge_pane`, `session_start_snippet`, `launch_intro_prompt`.

- Claude uses an stdout `Monitor`; Agy runs `giga watch --as <slug> --agy` via `run_command` plus a `WaitMsBeforeAsync` (`needs_bridge_pane()` is false); Codex needs a bridge pane (`needs_bridge_pane()` true) running `giga watch --as <slug> --codex`.
- The universal scaffolded filename is always `AGENTS.md`. Consumed by `config::agent_runtime` and by `init`/`launch`.

### Scaffolding & launch

#### [`init.rs`](./init.rs)
`giga init` — the idempotent scaffolder, and the owner of the AGENTS.md rendering pipeline.

- `pub fn run` is a thin wrapper for `pub fn run_with(config_path, do_trust)`. The flow: `Config::load` → canonicalize the config path (`init.rs:31-36`; v0.6.4 fix so relative `claudemd_template` resolves against the swarm dir, not a workdir symlink) → host-filter into `local_agents`/`skipped_agents`/`local_channels` (`init.rs:46-83`) → `mkdir` wsl/windows inboxes only where a local channel has that side (`init.rs:118-129`) → write each local channel's convention header via `fn render_channel_header` only when absent/empty (`init.rs:132-141`) → for each local agent: `to_host_fs(workdir)`, write AGENTS.md from `render_agent_claudemd`, scaffold codex bridge dirs, `#[cfg(unix)]` symlink `giga-harness.toml` for non-windows agents, copy `HANDOVER.md` once (`init.rs:162-246`) → optional `trust::pre_trust` → `registry::upsert` (`init.rs:262-277`) → print "next: giga launch".
- Key public items: `pub(crate) fn render_agent_claudemd(cfg, agent, config_dir, config_path)` (reused by `takeover.rs`), `fn inject_session_start` (replaces a `{{SESSION_START}}` placeholder, else a legacy `## Session Start` section, else returns the body unchanged), `fn render_swarm_boss_section`, `fn prepend_header`, `fn handover_template_for`.
- Invariants: existing non-empty channel files are kept (`[keep]`); AGENTS.md is **always** re-rendered so config changes propagate (the generated header warns operators their edits are overwritten — persist via the source template); `HANDOVER.md` is copied only on first init. The swarm_boss Monitor section is gated on `!cfg.hosts.is_empty()` (v0.3.7 Bug 10) so a local-only swarm doesn't spawn daemons that exit immediately and look crashed.

#### [`launch.rs`](./launch.rs)
`giga launch` — turns the config into terminal panes and spawns them.

- `pub fn run(config_path, skip_init, dry_run, only, new_window, terminal, stagger_per_agent_seconds, ui, ui_port)`: optionally runs `init::run` first → narrows agents by `--only` (hard-error on unknown names, `launch.rs:50-56`) and by `this_host` → `flat_map`s agents into `Pane`s. Codex agents get **two** panes (a `<agent>-bridge` running `giga watch --as <a> --codex` with `CODEX_CHANNEL_DIR` set, plus a `<agent>-cli`); their intro arrives as a synthetic `session-start` envelope written via `codex_channel::write_envelope` (v0.6.26), **not** on the CLI. Then decides daemon panes (`should_spawn_daemons_v2`) and an optional `giga-ui` pane, and dispatches to `terminal::launch`.
- Key public items: `pub(crate) fn intro_for_agent(intro, agent)` (extracted for testability), `fn should_spawn_daemons_v2`, `fn should_spawn_daemons`, `fn default_cmd_for_runtime`, `fn default_cmd_claude` (`claude -c --model M intro || claude --model M intro` — resume-or-fresh), `fn default_cmd_agy_interactive`, `fn default_cmd_tty_only`, `fn daemon_pane`.
- `should_spawn_daemons_v2`: false if no `[[hosts]]`, false if no peers, false if a local `swarm_boss` exists (it arms Monitors instead); otherwise true only when `--only` is empty (full bootstrap).
- INVARIANT: no intro string may contain backticks — they survive single-quoting through `wt.exe → wsl.exe → bash` and get command-substituted, corrupting the prompt (enforced by tests in `runtime.rs` and `launch.rs`). The `code_root` cd is deliberately **deferred** ("LATER") so agents don't immediately cd out of their workdir and lose sight of AGENTS.md/HANDOVER.md.

#### [`terminal.rs`](./terminal.rs)
Cross-platform multiplexer detection and spawning. `pub enum Multiplexer { WindowsTerminal, Tmux, MacTerminal, None }`, `pub struct Pane { title, cwd, cmd, platform, admin }`, `pub fn detect`, `fn decide_multiplexer` (pure precedence: in-tmux beats WT, else WT, else tmux, else None), `pub fn parse_override`, `pub fn launch`, and the four backends `launch_wt`/`launch_tmux`/`launch_mac_terminal`/`launch_print`.

- `launch_wt` splits regular vs admin panes; `stagger==0` builds one big `wt.exe` call joined by `;`, else one call per pane with `stagger_sleep`. WSL panes go through a temp `.sh` script (`bash -li <script>`) precisely to dodge the WT→wsl→bash quoting gauntlet (backtick-wrapped slugs would be command-substituted). `wt_spawn_or_explain` special-cases `ENOEXEC` (the 0-byte WindowsApps AppExecutionAlias stub) with a friendly install hint.
- `launch_tmux` kills any prior session on a non-incremental rebuild, attaches+adds windows when incremental. `MacTerminal` is opt-in only, never auto-detected. v0.6.25: inside tmux, `$TMUX` wins over `wt.exe` (always on PATH in WSL) to avoid a surprise WT window.

#### [`trust.rs`](./trust.rs)
Pre-populates Claude Code's per-project trust (`~/.claude.json` → `projects.<path>.hasTrustDialogAccepted = true`) so launched agents don't hit a folder-trust prompt. `pub fn pre_trust(cfg) -> Result<usize>` buckets project-keys by their target `.claude.json` path; `fn trust_target` resolves the (path, key) pair per platform (Windows agents → `/mnt/c/Users/<user>/.claude.json` via `fn extract_windows_user`); `fn update_claude_json` flips the flag, counting only changes.

- Idempotent (re-running reports 0 touches). Preserves all unrelated fields. Best-effort in `init`: a `pre_trust` error becomes a `[trust] warning` and does not fail init. `code_root` is also trusted so the agent's later cd doesn't prompt.

#### [`registry.rs`](./registry.rs)
The cross-swarm registry at `~/.giga/swarms.toml`. `pub struct Registry { entries }` (serde rename `swarms`), `pub struct Entry { name, config, code_roots, archived }`, `pub fn path`/`load`/`save` (tmp+rename atomic), `pub fn upsert`/`upsert_in`, `pub fn set_archived`, `pub fn find_by_cwd`/`find_match`, and the workhorse `pub fn resolve_config(provided)`.

- `resolve_config`: returns `provided` if it exists; returns an explicit non-default path unchanged (a missing explicit path is a user error); otherwise walks up parents for an ancestral `giga-harness.toml` (`registry.rs:187-200`, so `giga watch --as <slug>` works from a workdir), then tries `find_by_cwd` (code_roots match), else bails with a "run giga setup" message.
- `find_match` canonicalizes, requires `entry.config.exists()` (stale entries are silently skipped — self-healing, no gc command), and returns the **first** matching swarm. `archived` uses `serde(default, skip_serializing_if)` so old files stay readable. Heavy consumer: `main.rs` dispatch, plus `ui/`, `hosts`, `remote`, `sync`, `transports/git`.

#### [`cursor.rs`](./cursor.rs)
Two distinct cursor namespaces under `~/.giga`. Watch cursors `~/.giga/cursors/<agent>/<channel>.pos` (byte offset an agent has seen) via `cursor_path`/`read`/`write`; merge cursors `~/.giga/merge-cursors/<channel>/<slice_host>.pos` (bytes of each peer slice already appended) via `merge_cursor_path`/`read_merge`/`write_merge`. `pub fn giga_home` resolves `~/.giga` from `$HOME` else `%USERPROFILE%`.

- INVARIANT: cursor writes **never** crash a caller — every fs error is silently swallowed, because a failed cursor write must not kill the watcher/catchup/merger. Missing cursor → `None`; callers fall back to EOF (don't replay old messages as live) or 0 (full file for catchup). `giga_home` is the canonical resolver reused by `registry`, `launch` (ui.pid), `watch`, `merger`, `codex_channel`, `ui`.

### Messaging & coordination

#### [`post.rs`](./post.rs)
`giga post` — appends one canonically-formatted frame, enforcing the header/footer convention. `pub struct Args { channel, me, subject, body, waiting_on, needs, config, to, fyi }` (`body=None` reads from stdin), `pub fn run(args)`, and the load-bearing concurrency primitive `pub(crate) fn append_with_lock(path, bytes)` (also reused by `merger::append_bytes`). Internals: `fn append_plain` (fallback), `fn slice_path`, `fn format_block` (pure canonical frame), `fn resolve` (channel name → path).

- Routing (`post.rs:97-112`): on a cross-host channel (`!channel_is_local`), primary = `slice_path(this_host)` (errors if `this_host` unknown), secondary = the merged file. **Slice-first ordering** is the key invariant — the merged write is best-effort/warning-only, so a failed merged write still reaches peers via sync; merged-first would be silent divergence.
- `format_block` emits exactly three `===` lines; footer is `WAITING ON: who`, `WAITING ON: who (needs)`, or `(Informational, no response required.)`. `--fyi`+`--to` is rejected; the prefix `[fyi] ` / `[ack: a, b] ` is synthesized into the subject. v0.4.4 Bug 11: opens `read+write+create` (not append) so Windows `LockFileEx` gets `GENERIC_READ|GENERIC_WRITE`, then seeks to End inside the lock.

#### [`watch.rs`](./watch.rs)
`giga watch` — the always-running per-agent inbox watcher. `pub enum WatchMode { Default, Agy, Codex }`, `pub fn run_single` (legacy one-channel; rejects Codex), `pub fn run_multi(config_path, me, stagger_override, mode)` (config-aware, multi-channel). `struct ChannelState { name, path, last_size, participants, pending }`. Helpers: `fn busy_lock_path`, `fn agent_is_busy`, `fn refresh_tracked`, `fn is_header_line`, `fn is_waiting_on_me`, `fn run_stale_wait_scan`, `fn self_rearm`, `fn read_delta`.

- Main loop (`watch.rs:297`): 3s tick; every 5 ticks `refresh_tracked`; periodic stale-wait rescan. **Phase 1** (`329-441`) reads each channel's byte-delta and, per new header line not `[me] `, defers a notification or branches on broadcast prefix (`GigaRearm` → advance cursor + `self_rearm` via POSIX execve; `Fyi` → archive; `Ack` addressed → skip-if-not-me; `All`/none → fanout-stagger). **Phase 2** (`456-532`): if `agent_is_busy` → `continue` (buffer); else flush ready entries per mode and persist `cursor::write` **only** when a channel's pending is fully drained.
- Invariants: the cursor advances in-memory during read but persists only after emission while idle — crash-while-buffered re-delivers, never loses. Busy-lock deafness is the catastrophic mode, so every ambiguity resolves to idle/flush (a stale lock >300s reads idle). First-ever watch with no stored cursor starts at byte 0 (replays history); later sessions resume from EOF. Default broadcast stagger bumped 15→30 in v0.6.2.

#### [`merger.rs`](./merger.rs)
`giga merger` — the sole-writer daemon for cross-host channels. `pub fn run(config_path, once, quiet)`. `struct ChannelMergeState`/`SliceState`. `fn merge_tick`, `fn refresh_tracked`, `fn compute_active_channels`, `fn derive_slice_path` (mirrors `post::slice_path`), `fn read_delta`, `fn append_bytes` (delegates to `post::append_with_lock` to avoid torn frames).

- `merge_tick` (`merger.rs:112`): per channel/slice, stat len; if `cur < last_size` reset (truncation defense, `119-124`); if equal skip; else `read_delta` + `append_bytes` + advance + `cursor::write_merge` only on append success.
- `compute_active_channels` excludes `channel_is_local` channels and `retains()` to drop `this_host` so only **peer** slices are merged (v0.3.5 — `post` already dual-writes the own slice into merged, so re-merging would double-append). The sole-writer model is exactly why the merged file is append-only-safe for the watcher's `len()>last_size` invariant. Errors log to stderr and `continue` — the daemon never crashes.

#### [`sweep.rs`](./sweep.rs)
`giga sweep` — coordinator-side one-shot table of each channel's last message + open `WAITING ON` tag. `pub fn run(config_path, owed_by_filter)`, `struct Row`, `fn last_header_block`, `fn trunc`.

- Uses a **looser** header detection than `watch.rs`/`stale_wait.rs` (does not require the 20-byte timestamp tail). `WAITING ON` synonyms (`nobody`/`none`/`no-one`/`noone`/`n/a`/`informational`) collapse to `None`. Pure display; touches no cursors/slices/merger.

#### [`stale_wait.rs`](./stale_wait.rs)
Pure no-LLM stale-wait detection. `pub struct StaleWait { sender, subject, tag_timestamp, age_minutes }`, `pub fn scan(content, me, now, threshold_minutes)` (pure core), `pub fn scan_file` (best-effort wrapper, never crashes the watcher), `pub fn format_notification`, `fn parse_header`, `enum Footer { WaitingOn(String), Informational }`, `fn find_footer_in_message`.

- `scan` walks lines with a small resolution state machine: a `WAITING ON: me` from sender S is resolved by (a) me posting anything after, (b) S posting `WAITING ON: <other>`, or (c) S posting an informational closure; a newer `WAITING ON: me` from S supersedes its prior wait. Remaining `pending` entries past the threshold become `StaleWait`s sorted oldest-first. Future-dated tags (clock skew) clamp age to 0. Called from `watch.rs::run_stale_wait_scan`, which dedups on `(channel,sender,tag_timestamp)` so each supersede fires at most one notification.

#### [`codex_channel.rs`](./codex_channel.rs)
`giga codex-channel` — bridges inbox notifications into Codex's native filesystem-channel JSON inbox. `pub struct Args { me, channel_dir, config, catch_up, direct_only }`, `pub fn run`, `pub(crate) fn write_envelope(inbox_dir, swarm, me, channel, offset, text)` (reused by `watch.rs` Codex mode), `pub(crate) static SEQ`, `struct Envelope`, `fn refresh_tracked`.

- `write_envelope` assigns id `giga-<me>-<channel>-<offset>-<seq>`, writes a `.tmp` file, `sync_all`, then `fs::rename` to the final name — atomic publish so Codex never reads a partial file. `from="giga"`, `to="codex"`, `kind="brief"`, `idempotency_key=id` makes redelivery safe. `catch_up=false` starts at EOF (no replay); `direct_only` filters out `_`-prefixed broadcast channels. Cursor is written every tick (no busy-lock gate, unlike `watch.rs`).

### Agent / channel management & swarm boss

#### [`add_agent.rs`](./add_agent.rs)
`giga add-agent` — scaffolds a brand-new agent into an existing project. `pub struct Args { config, name, workdir, role, platform, peers, bench_scheduler, no_broadcast, template, dry_run, code_root, host, swarm_boss }`, `pub fn run(args)`. Helpers: `fn reject_tilde`, `fn preflight`, `pub struct DerivedChannel` (shared with `add_channel`), `fn derive_channels`, `fn find_broadcast_channels`, `fn append_agent`, `pub(crate) fn append_channel`, `fn append_to_broadcast`, `pub(crate) fn ensure_array_of_tables` (reused by `add_host`), `fn template_target`, `fn render_template`.

- Flow (`run` at `add_agent.rs:52`): `Config::load` → `preflight` → parse raw TOML into a `DocumentMut` (comments survive) → `append_agent`/`append_channel` per peer/`append_to_broadcast` → on `--dry_run` print and return **before** any disk write → write config, then write the `agents/<slug>.md` template only if absent → reload + `validate()` (post-write safety net) → if `--host` names a remote, best-effort `sync::bootstrap_peer` then `sync::run_remote_giga_init`.
- Gotchas: failure semantics are asymmetric — config is written **before** the template, so a mid-flight error can leave a written config (`preflight`'s relocated collision check at `350-373` prevents the common stray-template case). `~` is rejected hard because the launch `cd` shell-escapes the path so bash never expands the tilde. `swarm_boss` requires `platform=wsl` and at-most-one-per-host; channel side is forced to `windows` if either participant is windows. Broadcast detection is purely the `_` filename prefix.

#### [`add_channel.rs`](./add_channel.rs)
`giga add-channel --participants alice,bob` — appends a single bilateral (exactly 2-participant) channel. `pub struct Args { config, participants, file, dry_run }`, `pub fn run`, `pub(crate) fn derive(cfg, args)` (pure/testable; enforces exactly 2 participants, both in `[[agents]]`, alphabetical filename, `side=windows` if either participant is windows). Reuses `add_agent::{append_channel, DerivedChannel}`. Bilateral-only is a hard v1 constraint. A duplicate filename is a **hard error** (contrast `append_to_broadcast`, which is silently idempotent). Dispatched via `registry::resolve_config`.

#### [`set_swarm_boss.rs`](./set_swarm_boss.rs)
`giga set-swarm-boss <slug> [--unset] [--no-init]` — promotes/demotes the per-host supervisory `swarm_boss` role. `pub struct Args { config, slug, unset, no_init }`, `pub fn run`, `fn update_agent_swarm_boss_in_toml(config, slug, promote)` (sets `swarm_boss = true` on promote; **removes** the key entirely on demote so it reads as default).

- Validation mirrors `config.rs` load rules (boss must be `wsl`; at-most-one-per-host using the same host-bucket comparison: `(Some,Some)` compared, `(None,None)` same local bucket, mismatch = no collision). After the write, unless `--no_init`, it canonicalizes the path and calls `crate::init::run` to regenerate AGENTS.md. Unlike `add_agent`/`add_channel` it does **not** re-`Config::load` to re-validate — it trusts its own preflight + the init regen. Sibling pattern to `teleport::update_toml_agent_host` and `takeover::update_agent_runtime_in_toml`.

#### [`claude_operator.rs`](./claude_operator.rs)
`giga claude-operator` — a TTY-aware doc/launcher. `const DOC = include_str!("../templates/CLAUDE_OPERATOR.md")`, `pub fn run`. On a TTY it spawns `claude --append-system-prompt <DOC>` (inheriting stdio, exiting with the child's code via `std::process::exit`); off a TTY it prints `DOC` to stdout so a running agent's Bash tool captures it into context. No `Args` struct — the only switch is the stdout TTY check.

#### [`switch.rs`](./switch.rs)
`giga switch --runtime claude [--setup|--add|--list|<account>]` — swaps active Claude account credentials by copying real-file snapshots (no symlinks, because claude's `/login` and silent OAuth refreshes use write-temp-then-rename). `pub struct ClaudePaths` (decoupled from `dirs::home_dir()` so tests inject a `TempDir`), `pub struct Args`, `pub enum Op { Status, List, Setup, Add, Switch }`, `pub fn run` (two `cfg`-gated impls; the non-unix one bails), and the five `op_*` operations. `fn copy_cred_file` does atomic temp+fsync+rename and `chmod 0o600`.

- The correctness property: `op_switch` snapshots the **currently-active** creds back into the old account's file **first** (`switch.rs:208-215`) to capture any silent token refresh, then copies the target in. Unix-only by design; only `--runtime claude` is supported. Switching does **not** migrate running claude processes (they hold auth in memory) — the closing message tells the user to pkill + `giga launch` (tabs re-spawn as `claude -c`).

### Remote & multi-host

#### [`transport.rs`](./transport.rs)
The pluggable cross-host abstraction. `pub trait Transport: Send + Sync` with `name()`, `tick(&Config, this_host, dry_run)`, `bootstrap_peer(...)`, `supports_remote_exec() -> bool` (default false), `run_remote(...) -> Result<i32>` (default errors). `pub fn for_config(cfg) -> Result<Box<dyn Transport>>` dispatches by `cfg.transport.kind`, inferring `local` when `cfg.hosts.is_empty()` else `rsync+tailscale` (v0.2 back-compat).

- Any plug exposing remote exec must set `supports_remote_exec()=true` **and** override `run_remote` — only `RsyncTailscaleTransport` does, so `giga remote`/`sweep --host`/`launch --host` error cleanly under the git transport. Selection is centralized here; the concrete plugs live in [`transports/`](./transports/) and delegate their bodies back into `sync.rs`/`remote.rs`.

#### [`sync.rs`](./sync.rs)
The `giga sync` daemon plus the rsync-over-Tailscale-SSH engine, implementing REMOTE_DESIGN.md §4 slice-and-merge (each host pushes only what it **owns** — its own slice files + the canonical TOML; reception is symmetric, nobody pulls). `pub fn run`, `pub fn tick_once` (the `RsyncTailscaleTransport::tick` adapter), `pub fn compute_sync_plan` (pure planner), `pub fn bootstrap_peer`, `pub fn run_remote_giga_init`, plus `pub(crate)` helpers `backoff_for`, `build_rsync_target`, `remote_join`, `ssh_run` reused by `teleport`. `struct SyncCommand { peer_target, local_path, use_append_verify, kind }`. Consts: `POLL_INTERVAL=3s`, `RELOAD_EVERY_N_TICKS=5`, `MAX_BACKOFF=60s`, `SSH_TIMEOUT_OPTS`; `static QUIET`.

- `run` (`sync.rs:141`) exits if hosts empty, requires `cfg.this_host`, then loops: on `Err` increments failures and sleeps `backoff_for` (3→6→12→24→48→cap 60s); every 5 ticks reloads Config (Bug 11 — without it, post-launch `add-agent`/`add-channel` are invisible). `compute_sync_plan` (`343`) builds one `toml` command per peer, one `template` per local `agents/<name>.md`, and one `slice` (`use_append_verify=true`) per cross-host channel/peer pair. `execute` (`699`) is a no-op on missing local file (Bug 12 — avoids exit-23 spam every 3s).
- Invariants/gotchas: single-writer-per-slice at the wire (a host pushes only its own `<ch>.<this_host>.md`, never pulls/rewrites peer data); all remote paths forced to forward slashes (peer is always Linux/WSL); `SSH_TIMEOUT_OPTS` so a dead tailnet fails in ~10s not ~2min (v0.6.15); `quiet()` suppresses the per-tick summary for boss-hosted Monitors but **never** error lines. Canonical path comes from `cfg.source_path`, not a CWD-relative bare filename (F13). `bootstrap_peer` rsyncs the whole dir (excluding `*.local.toml`, `this_host.toml`, `workdirs/`) so templates + handover stubs propagate.

#### [`remote.rs`](./remote.rs)
`giga remote --host <host> <subcommand>` — the SSH-passthrough primitive behind `--host` sugar on launch/sweep. `pub fn run(args) -> Result<i32>` (gates on `transport.supports_remote_exec()`), `pub fn run_passthrough(cfg, peer, args)` (called by `RsyncTailscaleTransport::run_remote`), `fn lookup_host`, `fn build_ssh_target`, `fn build_remote_command`, `fn registry_config_dir`.

- Shells to `ssh <user>@<tailnet_hostname>` running `bash -lc 'cd <dir> && giga <args>'` with inherited stdio, returning `status.code().unwrap_or(255)`. The `bash -lc` wrapping is load-bearing (a non-interactive ssh otherwise lacks `~/.cargo/bin` on PATH). Auth is delegated to plain ssh (Tailscale SSH means no keypair exchange). All args + dir are `shell_escape`d and forward-slash normalized. Note: `remote.rs` lacks the `SSH_TIMEOUT_OPTS` that `sync.rs` uses, so a passthrough to a dead host can hang on the OS TCP timeout.

#### [`add_host.rs`](./add_host.rs)
`giga add-host` — appends a `[[hosts]]` entry and (by default) auto-bootstraps the peer; also handles the atomic first-host migration. `pub struct Args { config, name, tailnet_hostname, ssh_user, remote_config_dir, remote_inbox_dir, no_bootstrap, dry_run, this_host_name }`, `pub fn run`, `fn append_host`, `fn append_local_host`, `fn assign_local_host_to_unhosted_agents`, `fn resolve_local_host_name` (priority: `--this-host-name` → `$HOSTNAME` → `/etc/hostname` → error).

- On first migration (`cfg.hosts.is_empty()`) it also registers the local host, assigns `host=` to every previously host-less agent, and writes `this_host.local.toml`. After the write it reloads+revalidates via `Config::load` with **rollback**: on failure it restores the original TOML and removes the just-written `this_host` file. The bootstrap path (`sync::bootstrap_peer`) is best-effort — a locally-correct edit is never rolled back just because the peer is offline. First migration writes a **placeholder** local `tailnet_hostname == host name` (assumes MagicDNS); the operator is warned to edit it.

#### [`hosts.rs`](./hosts.rs)
`giga hosts` — read-only topology/roster inspector. `pub fn run` (per-swarm host/agent/channel tree), `pub fn run_list_all` (all registered swarms), `pub fn run_available` (registered hosts + unregistered tailnet members via `tailscale status --json`). `struct TailnetNode`, `fn query_tailscale_roster`/`invoke_tailscale_status_json` (PATH lookup then the `/mnt/c/.../tailscale.exe` fallback for WSL inheriting Windows-side Tailscale), `fn parse_tailscale_status` (pure, unit-tested: flattens `Self` + every `Peer`, strips trailing dot, skips nodes without `DNSName`), `fn extract_node`. The channel footer splits counts into cross-host (slice-and-merge) vs local-only (fast-path) via `channel_is_local`. Purely read-only.

#### [`setup_remote_node.rs`](./setup_remote_node.rs)
`giga setup --remote-node` — the on-the-peer installer. `pub struct Args { inbox_dir, dry_run, transport, repo }`, `pub fn run`, `fn run_tailscale` (6-step rsync+tailscale path), `fn run_git` (5-step git path), plus `wsl_check`, `ensure_rsync`, `inbox_dir_step`, `tailscale_logged_in`, `tailnet_hostname`, `default_inbox_dir`, and the `step(n, total, label, dry, f)` runner.

- `run_tailscale`: WSL check → ensure rsync → install Tailscale via official `install.sh` → `sudo tailscale up` (interactive) unless already logged in → `sudo tailscale set --ssh` (the linchpin that makes `giga remote --host` work without keypair exchange) → inbox dir → print the exact `giga add-host` command. Every step is idempotent. WSL-only for v1. The git path requires `--repo` and only smoke-tests auth (`git ls-remote`); the actual clone is deferred to the peer's first `giga sync` tick.

### Mobility & lifecycle

#### [`teleport.rs`](./teleport.rs)
`giga teleport <agent> --to <host>` — moves a running agent between tailnet hosts via an 8-step pipeline. `pub struct Args { agent, to, from, keep_running, dry_run, config }`, `pub fn run`, `struct Plan<'a>` (validated, borrows from Config), `fn preflight`, `pub(crate) fn build_ssh_target`, `fn rsync_direct` (with `fn rsync_two_hop` fallback), `fn prepend_banner_on_target`, `pub(crate) fn render_teleport_banner` (pure), `fn update_toml_agent_host`, `fn sync_toml_to_peers`, `fn run_remote_giga`, `fn kill_old_pane`.

- Steps (`run` at `teleport.rs:45`): touch source `HANDOVER.md` → rsync workdir (`--delete-after`, not `--delete`) → prepend a teleport banner → flip `agent.host` in TOML → `giga sync --once` → `giga remote ... init` → `giga remote ... launch --only <agent>` → `kill_old_pane` (unless `--keep-running`). Everything after preflight is best-effort with printed manual remediation.
- Homogeneous-path assumption: source and target `workdir` are the **same** `agent.workdir`. Channel slice files do **not** move — past posts stay in `<channel>.<source>.md`, new posts go to `<channel>.<target>.md`, preserving the append-only invariant. `~/.claude` history and giga cursors are per-machine, so the agent gets a full backlog replay on first watch tick. `kill_old_pane` shotguns all three window names (`<agent>`, `<agent>-cli`, `<agent>-bridge`) because it lacks the Config to know the runtime.

#### [`takeover.rs`](./takeover.rs)
`giga takeover [--as <slug>] [--to <runtime>]` — flips a single agent's runtime in place without moving hosts. `pub struct Args { config, as_agent, to_runtime, dry_run }`, `pub fn run`, `fn detect_slug_from_cwd`, `pub(crate) fn locate_session_file` (dispatches to per-runtime locators), `fn locate_claude_session`/`locate_agy_session`/`locate_codex_session`, `fn most_recent_jsonl`, `fn update_agent_runtime_in_toml`, `pub(crate) fn render_takeover_block`, `fn prepend_to_file` (atomic temp+rename), `pub(crate) fn takeover_prompt`.

- Flow: resolve slug (`--as` or `detect_slug_from_cwd`) → no-op if old==new runtime → flip TOML → reload Config → `init::render_agent_claudemd` (re-loaded Config so the new runtime's Session Start protocol is injected) → prepend a TAKEOVER block to `HANDOVER.md` → print a self-contained turn-1 prompt. Runtime/slug/role/channel memberships are unchanged — only the runtime flips.
- The Claude session-encoding rule (replace both `/` **and** `.` with `-`) is empirically verified with a dedicated unix-only regression test. The Codex locator uses a different encoding (`/` only) and is a best-effort guess. Session-log location is always best-effort; `None` is handled gracefully.

#### [`upgrade.rs`](./upgrade.rs)
`giga upgrade` — reinstalls the binary locally + on peers via the canonical `install.sh`/`install.ps1`, then posts a `[giga-rearm]` broadcast so running watchers reload. `pub struct Args { config, as_agent, skip_peers, skip_broadcast, skip_windows, dry_run }`, `pub fn run_bare(dry_run)` (swarm-less install; reached via `--bare` or when cwd isn't under a swarm), `pub fn run(args)`, plus `install_local*`, `install_local_windows_via_wsl_interop`, `install_remote`, `windows_pre_install_disarm`/`windows_post_install_rearm`, `resolve_fresh_giga_binary`, `infer_host_platform`, `resolve_default_posting_agent`. Consts `WINDOWS_OPERATOR_WAIT_SECS=15`, `WINDOWS_AGENT_REARM_DELAY_SECS=60`.

- Windows in-place overwrite of a running `giga.exe` fails with a sharing violation, hence the disarm/wait/install/rearm dance: a targeted `[ack:<slugs>]` pre-install disarm broadcast (only Windows agents act, via the watcher's fanout filter at `watch.rs ~347`), wait, install, then a matching post-install rearm. The agent rearm delay (60s) **must** exceed operator wait (15s) + install + buffer.
- Critical v0.6.20 fix: after `install_local` on Linux, `install.sh` unlinks the running binary, so `current_exe()` resolves to a `(deleted)` path that fails with ENOENT — `resolve_fresh_giga_binary` rebinds via `which::which("giga")` for all subsequent subprocess spawns. The final `[giga-rearm]` is silently self-handled by v0.6.3+ watchers via in-place execve (no agent turn/API call).

#### [`setup.rs`](./setup.rs)
`giga setup` — a zero-state bootstrap. It does **not** scaffold anything in Rust; it shells out to `claude` with a large baked-in prompt that turns Claude Code into a guided bootstrap agent. `pub fn run`, `fn current_platform_hint` (compile-time `cfg!` OS string), `fn build_prompt(cwd, configs_default, platform_hint)` (single `format!`, extracted so tests assert all placeholders interpolated). The prompt embeds the compiled-in giga version via `env!("CARGO_PKG_VERSION")` so it always encodes that release's exact command surface.

- The prompt encodes the 7 setup questions, the canonical design/code/test/review role boundaries, the `workdir != code_root` invariant, alphabetical bilateral channel filenames, `_`-prefixed broadcast channels, and the validate→init→launch sequence. Crucially it instructs the bootstrap agent to emit a literal `{{SESSION_START}}` placeholder (not hand-written Monitor instructions) so `giga init` injects the runtime-correct protocol — guarded by a dedicated test. This is the only one of the lifecycle four that is claude-specific and has no Config/runtime/sync dependency.

## Data & control flow

**Cold start.** `main.rs` dispatches `Init` → `init::run_with`, which loads `Config`, host-filters, writes channel headers (`post`-convention) and per-agent `AGENTS.md` (rendered from `templates` + `runtime` snippets), optionally calls `trust::pre_trust`, and registers the swarm via `registry::upsert`. `Launch` → `launch::run` re-loads the Config, builds `terminal::Pane`s (two per codex agent, plus optional `giga-sync`/`giga-merger`/`giga-ui` daemon panes decided by `should_spawn_daemons_v2`), and hands them to `terminal::launch`.

**Steady-state messaging.** An agent runs `giga post` (`post::run`), which appends a `format_block` frame under an exclusive lock. On a local-only channel it writes the merged file directly (fast path); on a cross-host channel it writes its own slice first (`slice_path(this_host)`) then best-effort the merged file. Each agent's `giga watch` (`watch::run_multi`) tails its channels on a 3s tick, gates emission on the `~/.giga/busy/<me>.lock`, applies broadcast filtering/stagger, runs `stale_wait::scan_file`, and delivers via Claude stdout / agy exit / codex envelope (`codex_channel::write_envelope`). Read offsets persist through `cursor`, written only **after** emission so a crash re-delivers rather than loses.

**Cross-host.** `giga sync` (`sync::run`) pushes each host's own slices + canonical TOML to peers over rsync/Tailscale-SSH; `giga merger` (`merger::run`) folds **peer** slice files into the local merged file as ordinary appends (sole-writer → the watcher's `len()>last_size` invariant holds). Both use the merge-cursor namespace in `cursor`. `transport::for_config` selects the plug; `remote::run_passthrough` powers `--host` sugar. Topology changes go through `add_agent`/`add_channel`/`add_host`/`set_swarm_boss`, which edit the TOML via `toml_edit` (comments preserved) and re-validate through `Config::load`; cross-host additions auto-bootstrap via `sync::bootstrap_peer` + `sync::run_remote_giga_init`.

**Lifecycle.** `teleport`, `takeover`, `upgrade`, and `setup` mostly re-invoke the `giga` binary as a subprocess (`giga remote`, `giga sync --once`, `giga post`, `giga init`, `giga launch`) rather than calling those modules in-process. `takeover` and `teleport` reuse `init::render_agent_claudemd`; `set_swarm_boss` calls `init::run` directly.

## Cross-references

Subfolders (documented separately):
- [`transports/`](./transports/) — concrete `Transport` plugs: `local.rs` (no-op single-host), `rsync_tailscale.rs` (thin adapter delegating to `sync.rs`/`remote.rs`), `git.rs` (state-repo). `mod.rs` wires them up.
- [`ui/`](./ui/) — the axum + websocket web dashboard (`giga ui`): `api.rs`, `ws.rs`, `server.rs`, `process.rs`, `channel.rs`, `pid.rs`, `state.rs`, `mod.rs`.

Project docs:
- [`../ARCHITECTURE.md`](../ARCHITECTURE.md) — the system-wide architecture hub (coordination model, module map, command lifecycle, on-disk layout, glossary).
- Top-level [`../README.md`](../README.md) — operator-facing overview.
- [`../docs/QUICKSTART.md`](../docs/QUICKSTART.md), [`../docs/MANUAL_SETUP.md`](../docs/MANUAL_SETUP.md), [`../docs/COMMAND_REFERENCE.md`](../docs/COMMAND_REFERENCE.md), [`../docs/REMOTE_QUICKSTART.md`](../docs/REMOTE_QUICKSTART.md).

Design docs:
- [`../design/REMOTE_DESIGN.md`](../design/REMOTE_DESIGN.md) / [`../design/REMOTE_DUAL_WRITE_DESIGN.md`](../design/REMOTE_DUAL_WRITE_DESIGN.md) — slice-and-merge, dual-write (`post`, `merger`, `sync`).
- [`../design/TRANSPORT_DESIGN.md`](../design/TRANSPORT_DESIGN.md) — pluggable transport trait + the three plugs (`transport`, `setup_remote_node`).
- [`../design/BROADCAST_FANOUT_DESIGN.md`](../design/BROADCAST_FANOUT_DESIGN.md) — broadcast prefixes + stagger (`config`, `watch`, `upgrade`).
- [`../design/SWARM_BOSS_DESIGN.md`](../design/SWARM_BOSS_DESIGN.md) — per-host single-boss supervisory model (`set_swarm_boss`, `init`, `launch`, `sync` `--quiet`).
- [`../design/TELEPORT_DESIGN.md`](../design/TELEPORT_DESIGN.md) — teleport/takeover flow (`teleport`, `takeover`).
- [`../design/STALE_WAITS_NO_LLM_DESIGN.md`](../design/STALE_WAITS_NO_LLM_DESIGN.md) — no-LLM stale-wait detection (`stale_wait`, `watch`).
