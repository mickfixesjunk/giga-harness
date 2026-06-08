# Manual setup

This is the hand-driven walkthrough — every step you'd type, every file you'd write — plus the field-by-field reference for `giga-harness.toml`. It's the right doc to read if you want to understand how the pieces fit, are debugging an unusual setup, or prefer to author the config yourself instead of delegating the bootstrap to an agent.

For the agent-driven flow, just run `giga setup` from your project directory — Claude Code opens with a baked-in prompt that does everything below for you. See [../README.md](../README.md). This doc is the reference of what `giga setup` is doing under the hood. (Binary `giga`, v0.6.54.)

> **Single-host scope.** This walkthrough covers a swarm that runs entirely on one machine — today's default and most common case. The config schema below is complete (it documents the multi-host tables too), but the worked example is single-host. Once you've got a working single-host swarm and want to add a second machine on a tailnet, see [REMOTE_QUICKSTART.md](REMOTE_QUICKSTART.md).

## Background

`giga` is a CLI for running N parallel agent sessions that coordinate via append-only Markdown files. One terminal tab per agent; each tab opens in the agent's workdir with the agent CLI already running. Agents post to shared inbox files and a single `Monitor` per agent (`giga watch --as <slug>`) tails every channel that agent participates in. New messages become notifications. No MCP server, no message bus, no service to keep up — just files.

The default agent runtime is **Claude Code** (`claude`). giga also supports **Codex** and **Antigravity** (`agy`), selectable per-swarm via `[project].runtime` or per-agent via `[[agents]].runtime` (values `claude` | `codex` | `agy`; `antigravity` is an alias for `agy`). They launch differently — a codex agent spawns two panes (a CLI pane + a bridge pane) — but the coordination model is identical. The worked example below uses the default Claude runtime; everything generalizes.

Sample message:

```
===
[design] T2.1 spec handed to test — 2026-05-22T10:14:00Z
===

Scope agreed. Implementation up to code.

WAITING ON: test (spec walkthrough)
===
```

The `WAITING ON: <agent>` / `(Informational, no response required.)` footer is load-bearing — `giga sweep` reads it to tell you who owes whom, and ambiguous closings stall pipelines.

## Walkthrough — 3 agents (design / test / code)

A complete worked example: bootstrap a project where `design` scopes features, `code` implements, and `test` verifies. Pipeline:

```
                   ┌──────────┐
        ┌─────────►│   test   │◄─────────┐
        │          └──────────┘          │
        │               ▲                │
        │               │                │
   ┌────┴─────┐         │          ┌─────┴────┐
   │  design  │─────────┼─────────►│   code   │
   └──────────┘                    └──────────┘
```

Three bilateral channels + one broadcast for all-hands announcements.

### 1. Scaffold the project directory

(Install giga first if you haven't — see [../README.md](../README.md#install).)

The canonical location for swarm configs is `~/.giga/configs/<project>/`:

```sh
mkdir -p ~/.giga/configs/myproj/{agents,workdirs/design,workdirs/code,workdirs/test}
cd ~/.giga/configs/myproj
```

(You don't need to pre-create the `inbox/` directory — `giga init` creates it. See the `[paths]` note below.)

> Each agent gets its own **workdir** (where `AGENTS.md` lives and the agent CLI launches) but they can share a **code_root** — the directory the agent actually edits. This keeps the launch context clean while letting multiple agents collaborate on one codebase.

### 2. Write `giga-harness.toml`

The minimal valid config is `[project]` + `[[agents]]` + `[[channels]]`. Here's the full worked example (a per-field schema reference follows in [§ Config schema reference](#config-schema-reference)):

```toml
[project]
name = "myproj"
# Optional: description, launch_model (default "claude-opus-4-7"),
# launch_intro_prompt, runtime — see the schema reference.

# [paths] is OPTIONAL since v0.6.24.
# wsl_inbox defaults to <config_dir>/inbox, so for a config living at
# ~/.giga/configs/myproj/giga-harness.toml you can omit this block entirely.
# Set it only to override the default location:
# [paths]
# wsl_inbox = "/home/me/some/other/inbox"

# ---------- agents ----------
# Each agent has an isolated workdir but shares one code_root.

[[agents]]
name = "design"
workdir = "/home/me/.giga/configs/myproj/workdirs/design"
code_root = "/home/me/code/myproj"
role = "Scope features. Decide what gets built and in what order."
platform = "wsl"
claudemd_template = "agents/design.md"

[[agents]]
name = "code"
workdir = "/home/me/.giga/configs/myproj/workdirs/code"
code_root = "/home/me/code/myproj"
role = "Implement the spec. Talk to test for verification."
platform = "wsl"
claudemd_template = "agents/code.md"

[[agents]]
name = "test"
workdir = "/home/me/.giga/configs/myproj/workdirs/test"
code_root = "/home/me/code/myproj"
role = "Write tests against the spec. Verify code's implementation."
platform = "wsl"
claudemd_template = "agents/test.md"

# ---------- channels ----------

[[channels]]
file = "code-design.md"
side = "wsl"
participants = ["code", "design"]
purpose = "Spec questions, scope refinements, implementation tradeoffs."

[[channels]]
file = "code-test.md"
side = "wsl"
participants = ["code", "test"]
purpose = "Implementation ↔ verification handoffs."

[[channels]]
file = "design-test.md"
side = "wsl"
participants = ["design", "test"]
purpose = "Test-coverage decisions, edge-case scoping."

[[channels]]
file = "_broadcast.md"
side = "wsl"
participants = ["design", "code", "test"]
purpose = "Ecosystem-wide announcements (migrations, standing directives, agent lifecycle)."
```

### 3. Write per-agent templates

Each `claudemd_template` is the source for that agent's **`AGENTS.md`** — the file giga renders into the agent's workdir, which the agent reads on startup to know who it is. (`AGENTS.md` is universal across all runtimes — claude, codex, agy — and is **always overwritten on every `init`/`launch`**. Persistent edits go in the source template here, never in the workdir copy.)

Minimal version of `agents/design.md`:

```markdown
# design agent

You are the **scope owner** for myproj. Decide what gets built. Hand specs to `code`. Decide what gets tested with `test`. Get explicit greenlights before either side starts work.

## Session Start

1. Post intro on each of your channels: `giga post <channel> --as design --subject "online" --body "design session resumed"`. Informational.
2. Arm the Monitor below. Exact command.
3. Standby.

## Channels you watch

​```
Monitor(persistent: true, command: "giga watch --as design")
​```

One watcher auto-discovers every channel where you participate (per `giga-harness.toml`). New channels added later are picked up automatically (~15s reread).

## Convention

Every message ends with either:
* `WAITING ON: <agent> (<what's needed>)`
* `(Informational, no response required.)`

Ambiguous closings stall the pipeline.
```

`agents/code.md` and `agents/test.md` follow the same shape — change the slug in the Monitor command and the role description in the header. Once you have all three, you're ready.

### 4. Validate, scaffold, launch

```sh
giga validate
giga init       # creates inbox files + renders each agent's AGENTS.md in their workdir
giga launch     # opens 3 terminal tabs, one per agent, each with claude already running
```

> **Run `init` from the swarm dir.** Unlike `launch`/`sweep`/`watch`/`post`, the `init` and `add-agent` commands use the `--config` path **literally** — they do NOT resolve it through the registry. From a directory with no `giga-harness.toml`, `giga init` fails even though `giga launch` would have found the config. Either `cd` into the swarm dir or pass an explicit `--config <path>`.

That's it. Each agent reads its `AGENTS.md`, arms its `giga watch --as <slug>` watcher, posts a one-line "I'm online" intro on each of its channels, and waits for the other sides to talk. The first time `code` finishes a chunk, it posts on `code-design.md` and `design`'s watcher fires.

## Config schema reference

`giga-harness.toml` is parsed into `config::Config` by `Config::load(path)`, which reads + parses the TOML, canonicalizes the path, loads the sibling `this_host` identity file (see below), applies inbox-path defaults, then validates. **Hard-required keys:** `[project].name`; each `[[agents]]` `name`/`workdir`/`role`; each `[[channels]]` `file`/`side`/`participants`. The schema is strongly backward-compatible — `[project]` + `[[agents]]` + `[[channels]]` is a complete local-only swarm; every other table is opt-in.

### `[project]`

| Field | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | **required** | Swarm name. Used in the `windows_inbox` default path, the tmux/wt session name (`giga-<name>`), the registry key, and channel headers. |
| `description` | string | — | Free text. |
| `launch_intro_prompt` | string | — | Opening prompt for every spawned CLI at launch. When set, overrides the per-runtime built-in intro for ALL agents. |
| `launch_model` | string | `"claude-opus-4-7"` | Passed to `claude --model`. Claude-only. Override for cheaper agents. |
| `runtime` | enum `claude`/`codex`/`agy` | → `claude` | Swarm-wide runtime. Per-agent override available (`antigravity` is accepted as an alias for `agy`). |

### `[paths]` (optional since v0.6.24)

| Field | Type | Default | Meaning |
|---|---|---|---|
| `wsl_inbox` | path | `<config_dir>/inbox` | Dir holding `side = "wsl"` channel files. Required (after the default) only if a channel is `side = "wsl"`. |
| `windows_inbox` | path | `<USERPROFILE>\.giga\configs\<project>\inbox` (resolved via `cmd.exe` interop on WSL) | Dir for `side = "windows"` channels. Use forward slashes. May stay unset on pure Linux without WSL interop. |

Because `wsl_inbox` defaults to `<config_dir>/inbox`, a config that lives at `~/.giga/configs/myproj/` can omit `[paths]` entirely and inbox files land in `~/.giga/configs/myproj/inbox/`. Hand-writing the default just risks drift if you ever move the config.

### `[[agents]]`

| Field | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | **required** | Agent slug; referenced by channel `participants` and `bench_protocol.scheduler`. |
| `workdir` | path | **required** | Launch context dir (the terminal's cwd; where `AGENTS.md`/`HANDOVER.md` live). |
| `role` | string | **required** | Role description, injected into `AGENTS.md`. |
| `platform` | string | `"wsl"` | `wsl` or `windows`. Controls terminal spawning + trust-file targeting. |
| `host` | string | → `this_host` | Which `[[hosts]].name` this agent runs on. **Required on every agent once `[[hosts]]` is non-empty.** Omit for single-host. |
| `bench_scheduler` | bool | `false` | Marks the bench scheduler; **at most one per swarm**. |
| `claudemd_template` | path | → generated minimal | Per-agent `AGENTS.md` template, path relative to the config dir. |
| `launch_cmd` | string | → per-runtime default | Override the shell command spawned in the terminal. |
| `admin` | bool | `false` | Request UAC elevation for the `wt.exe` tab (Windows only; ignored elsewhere). |
| `code_root` | path | — | Where the agent edits code (distinct from `workdir`); injected into `AGENTS.md` + intro and pre-trusted. |
| `swarm_boss` | bool | `false` | Runs the cross-host sync+merger daemons via `AGENTS.md` Monitors. **At most one per host; must be `platform = "wsl"`.** Only relevant in multi-host swarms. Managed with `giga set-swarm-boss`. |
| `runtime` | enum `claude`/`codex`/`agy` | → `[project].runtime` → `claude` | Per-agent runtime override. |

### `[[channels]]`

| Field | Type | Default | Meaning |
|---|---|---|---|
| `file` | string | **required** | Filename only (the directory comes from `paths.<side>_inbox`). `_*.md` = broadcast; bilateral channels use sorted `<a>-<b>.md`. |
| `side` | string | **required** | `wsl` or `windows`; selects the inbox dir. The matching inbox path must be set. |
| `participants` | array&lt;string&gt; | **required** | Agent names on the channel (usually 2; more for broadcast). Each must resolve to a `[[agents]].name`. |
| `purpose` | string | — | Free-text description, included in the generated channel-file header. |
| `stale_wait_threshold_minutes` | integer | → `[watch].stale_wait_threshold_minutes` | Per-channel override of the stale-wait threshold. |

### Optional tables

| Table.field | Default | Meaning |
|---|---|---|
| `[broadcast].stagger_seconds` | `30` | Per-slot fanout delay for `_*.md` channels (smooths the per-account rate-limit hit). `0` = instant. |
| `[broadcast].default_recipients` | `"all"` | Treat unprefixed broadcasts as `[all]`. Only `all` is wired through today. |
| `[watch].stale_wait_threshold_minutes` | `30` | How old an unresolved `WAITING ON: <me>` must be before the watcher surfaces it. |
| `[watch].stale_wait_recheck_seconds` | `60` | Re-scan cadence after the arm-time scan. `0` disables periodic re-scan (the arm-time scan still runs). |
| `[bench_protocol].scheduler` | **required when table present** | Agent that schedules bench slots (see [§ Bench coordination](#bench-coordination)). Pair with `bench_scheduler = true` on that agent. |
| `[bench_protocol].slot_pool` | `"this-host"` | `this-host` (shared pool) or `per-host`. |
| `[transport].kind` | inferred (`local` if no `[[hosts]]`, else `rsync+tailscale`) | Active transport: `local` / `rsync+tailscale` / `git`. Multi-host concern — see [REMOTE_QUICKSTART.md](REMOTE_QUICKSTART.md). |
| `[[hosts]]` | empty (local-only) | One entry per machine in a multi-host swarm (`name` + `tailnet_hostname` required). Multi-host concern — see [REMOTE_QUICKSTART.md](REMOTE_QUICKSTART.md). |

### Windows + WSL on one machine

The worked example is WSL-only, but a single machine can run both WSL and Windows-native agents over the WSL/Windows interop boundary. To do it: set `platform = "windows"` on the Windows agents, give their channels `side = "windows"`, and make sure `[paths].windows_inbox` is set (it defaults via `cmd.exe` interop on WSL). Windows agents that need an elevated `wt.exe` tab set `admin = true`.

### Sibling identity file (`this_host.local.toml`) and the `.local.toml` convention

`this_host` is **not** a field in `giga-harness.toml`. It lives in a sibling file next to the config:

- **`this_host.local.toml`** (preferred) or legacy `this_host.toml`, with a single key:
  ```toml
  this_host = "<host-name>"
  ```
- It names which `[[hosts]].name` is THIS machine. It's irrelevant for a single-host (local-only) swarm and you don't need to create it; it becomes load-bearing the moment you add `[[hosts]]`.
- The **`.local.toml` suffix is a project-wide convention meaning "host-private — never rsync'd to peers."** Cross-host bootstrap explicitly excludes `*.local.toml` when it pushes the config dir, so each machine keeps its own identity file. Use the same suffix for any other config you want to stay local to one host.

`source_path` (the canonical absolute path of the loaded config) is likewise computed at load time, not written in the TOML.

## Adding an agent

You can add a `review` agent that audits code changes mid-flight:

```sh
giga add-agent \
  --name review \
  --workdir /home/me/myproj-review \
  --role "Review code changes against the design spec. Surface deviations to design." \
  --peer design \
  --peer code
```

What this does in one shot:

- Appends `[[agents]] review` to `giga-harness.toml`.
- Appends `[[channels]] code-review.md` and `design-review.md` (alphabetical filename convention; `--peer` is repeatable, not comma-delimited).
- Appends `review` to `_broadcast.md` participants so they get all-hands announcements.
- Writes `agents/review.md` with a minimal stub template.
- Re-validates the config.

Use `--dry-run` first to preview. Use `--template <path>` to supply a fleshed-out `AGENTS.md` instead of the stub. Like `init`, `add-agent` uses the `--config` path **literally** — run it from the swarm dir (or pass an explicit `--config`).

Then bring `review` online without disrupting the running 3-agent tabs:

```sh
giga launch --only review --new-window
```

The new wt window pops up for `review`. The three existing tabs keep running. `review`'s single-Monitor watcher auto-discovers its two bilateral channels + the broadcast on its first tick. `design` and `code` are already on the same auto-discovery watcher (`giga watch --as <slug>`), so they pick up the new bilateral channels (`design-review.md`, `code-review.md`) on their next config reread (~15s) — no manual re-arming.

**Safe to run from inside an agent's session.** If `design` is the natural orchestrator, you can say "design, please add a review agent that..." and they can run `giga add-agent` themselves. Launch stays your call (window-layout intent is yours).

### Hand-adding a single channel

If you just want one new bilateral channel between two existing agents (without adding an agent), use:

```sh
giga add-channel --participants code,test
```

`--participants` is comma-separated and must name **exactly two** existing agents (v1 is bilateral-only). The filename is auto-derived as the sorted `<a>-<b>.md`; pass `--file <name>` to override. `--dry-run` previews. This is the command companion to hand-authoring a `[[channels]]` block — both produce the same result, and the watchers pick the new channel up within ~15s.

## Standing an agent down

When you want to pause an agent without losing the ability to reactivate cleanly. **Prefer this over removal** — stand-down preserves history, makes reactivation a 30-second edit, and keeps every channel structure intact.

### Stand-down (recommended)

1. **Announce on `_broadcast.md`:**

```sh
giga post _broadcast.md --as design --subject "stand-down: review" \
  --body "Standing review down as of $(date -u +%Y-%m-%d). No active audit work in the queue. Their watcher stays armed in standby mode — they won't respond on their bilaterals. To reactivate later: restore the template + role and re-launch.
(Informational, no response required.)"
```

2. **Edit `giga-harness.toml`:**
   - Keep the `[[agents]]` entry.
   - Update their `role`: `"Stood down. Watcher armed but channel inactive — only triggers if reactivated."`
   - Keep all `[[channels]]` blocks listing them as participant (so reactivation is one edit).
   - Keep them in `_broadcast.md` participants (they should still receive announcements if reactivated).

3. **Replace `agents/review.md` with a minimal standby template:**

```markdown
# review agent (stood down)

You are currently **stood down**. You exist to keep the channel structure intact for possible reactivation; you do not initiate work.

## Session Start

1. Read `./HANDOVER.md` if it exists.
2. Arm `Monitor(persistent: true, command: "giga watch --as review")`.
3. Standby. If a message arrives, read it. If it asks you to do work, reply: "I'm currently stood down. Confirm with the user before I resume." Don't act without confirmation.
```

4. **Refresh + close the agent's tab:**

```sh
giga init        # re-renders agents/<slug>.md → workdir AGENTS.md
# close review's terminal tab manually
```

To reactivate later: restore the canonical template + role line in TOML, run `giga init`, then `giga launch --only review --new-window`. They come back online with all channels intact.

### Full removal (rare)

Only when the role is dissolved — repo deleted, product surface killed, etc.

```sh
# 1. Announce on _broadcast.md ("removed" rather than "stood down" so peers don't expect reactivation).

# 2. Edit giga-harness.toml:
#    - delete the [[agents]] block for the removed slug
#    - delete every [[channels]] block listing them as participant
#    - remove them from _broadcast.md participants

# 3. Delete the canonical template:
rm agents/<slug>.md

# 4. Validate (should pass — no dangling participants):
giga validate

# 5. Close the agent's tab.
```

Inbox files for the deleted channels stay on disk as history. `giga init` won't recreate them now that the config doesn't list them; it also won't delete them. Archive or `rm` them manually if you want a clean inbox dir.

For the broader operational guide, see [QUICKSTART.md](QUICKSTART.md).

## Subcommands

The commands you'll touch most during manual setup:

| Command | What it does |
|---------|--------------|
| `giga setup` | One-command bootstrap. Launches Claude Code with a baked-in prompt that walks you through scaffolding a new swarm end-to-end. Run from your project directory. |
| `giga validate [config]` | TOML schema check + cross-reference. Also flags orphan channel files on disk that aren't enrolled in `[[channels]]`. No side effects. |
| `giga init [config]` | Host-aware scaffolder (idempotent). Creates inbox dirs + channel files (existing inbox files are kept), always re-renders each agent's `AGENTS.md`, writes `HANDOVER.md` on first init only, pre-populates Claude per-folder trust (skip with `--no-trust`), creates a workdir→config symlink (a `giga-harness.toml` symlink in each agent workdir pointing at the canonical config, so bare `giga` commands resolve it), and registers the swarm in `~/.giga/swarms.toml`. **Uses the `[config]` path literally — run from the swarm dir.** |
| `giga add-agent --name X --workdir Y --role "..." [--code-root Z] --peer A [--peer B]` | Scaffold a new agent — appends `[[agents]]` + per-peer `[[channels]]`, adds the slug to any `_*.md` broadcast channel, writes `agents/<slug>.md`. `--dry-run` previews; `--template <path>` supplies a fleshed-out template. Uses `--config` literally; safe to run from within an agent's session. |
| `giga add-channel --participants a,b` | Append one bilateral channel between two existing agents (`--file` to override the auto name; `--dry-run` previews). |
| `giga launch [config]` | One terminal per agent. `--terminal <mode>`: `auto` (default — wt > tmux > print), `tmux`, `mac-terminal` (alias `mac`), `wt` (alias `windows-terminal`), `print` (alias `none`). `--only <a,b>` spawns just the named agents (non-disruptive add). `--new-window` forces a fresh wt window. `--dry-run` prints the plan without spawning. `--skip-init` skips the implicit `init`. `--stagger-per-agent-seconds <n>` (default `0`) spaces out CLI starts — use 5–15s for 10+ agents. Config resolves via: explicit `[config]` arg → ancestral `giga-harness.toml` → `~/.giga/swarms.toml` registry. |
| `giga sweep [config]` | Tabulate every channel's last message + open `WAITING ON` tags. `--owed-by <agent>` filters to channels where that agent is the one being waited on. |
| `giga post <channel> --as <agent> --subject ... [--body ... \| stdin] [--waiting-on <agent> [--needs ...]]` | Append a properly-formatted message. Validates that the sender is a participant. `<channel>` accepts bare names (`pipeline-usage`) or the full `.md` form, positionally or via `--channel`. |
| `giga watch --as <agent> [<channel>]` | Long-running watcher (use under Claude Code's `Monitor` tool). Without `<channel>`: config-aware multi-channel mode — auto-tracks every channel where `<agent>` is a participant and picks up new channels added later (~15s reread). With `<channel>`: legacy single-file mode. |

There are 22 user subcommands in total — including `set-swarm-boss`, `takeover` (flip an agent's runtime in place), `switch` (swap the active Claude account), `ui` (browser dashboard), and the multi-host family (`add-host`, `sync`, `merger`, `remote`, `teleport`). For the full surface see [COMMAND_REFERENCE.md](COMMAND_REFERENCE.md) or run `giga --help`.

## Resuming after a reboot

`giga init` registers each swarm in `~/.giga/swarms.toml`, mapping `code_root` → config path. Any subsequent `giga launch` / `validate` / `sweep` / `watch` / `post` resolves the config via that registry — so you can `cd` to any directory under any agent's code root and run the command without `--config`:

```sh
cd ~/code/myproj
giga launch     # finds ~/.giga/configs/myproj/giga-harness.toml via the registry
```

The resolver also walks up from cwd looking for an ancestral `giga-harness.toml` *before* consulting the registry. This matters when you're already inside an agent's workdir (e.g. `~/.giga/configs/<swarm>/workdirs/<slug>/`) — the workdir tree isn't a `code_root`, so the registry lookup wouldn't find anything, but the ancestor walk lands on the config two levels up. Net effect: `giga watch --as <slug>` just works from a freshly-launched agent terminal.

If no swarm is registered for the current directory (or any ancestor) *and* no ancestral `giga-harness.toml` exists, giga prints a clear error pointing you to `giga setup`.

> **Exception:** `giga init` and `giga add-agent` do NOT route through the registry — they use the `--config` path literally. Run them from the swarm dir or pass an explicit `--config`. Only `launch`/`sweep`/`watch`/`post`/etc. resolve via the registry.

## The convention

`giga` enforces one rule: every channel message ends with either

```
WAITING ON: <agent-name> (<what they need to do>)
```

or

```
(Informational, no response required.)
```

`giga sweep` reads these tags to tell you who owes whom. Ambiguous closings — "I'll consider this agreed", "let me know if you have concerns" — stall pipelines. The convention removes that whole class of failure. `giga post` writes the header + footer for you so agents can't forget.

## Bench coordination

Bench coordination is a convention layered on top of channels: agents post `bench-request <slot>` and wait for `bench-clear <slot>` from a designated scheduler before doing heavy work (e.g. a long benchmark that needs exclusive access to a resource). To wire it in, add a `[bench_protocol]` table naming the `scheduler` agent and set `bench_scheduler = true` on that agent (at most one per swarm):

```toml
[bench_protocol]
scheduler = "design"
# slot_pool = "this-host"   # or "per-host"
```

```toml
[[agents]]
name = "design"
# ...
bench_scheduler = true
```

## Architecture

* **Agents** run wherever you want — WSL, Windows-native, remote SSH, doesn't matter. Each one is just an agent CLI session (claude/codex/agy) in a terminal.
* **Channels** are plain text files in shared inbox directories. Both `side = "wsl"` and `side = "windows"` are supported on the same machine via the WSL/Windows interop boundary (see [§ Windows + WSL on one machine](#windows--wsl-on-one-machine)).
* **Watchers** are `giga watch --as <agent>` processes (one per agent), run under Claude Code's `Monitor` tool with `persistent: true`. One watcher per agent — it reads the config, tracks every channel that agent participates in, and rereads the config every ~15s so newly-added channels appear without restarting. Each new message becomes one stdout line (`inbox <channel>: ...`), which Claude Code treats as a notification.

There is deliberately no central service. If giga itself crashes, the agents keep talking.

License (MIT): see [LICENSE](../LICENSE). Subcommand cheat sheet also lives in the [README](../README.md) and the full [COMMAND_REFERENCE.md](COMMAND_REFERENCE.md).
