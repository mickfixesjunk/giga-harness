# Manual setup

This is the hand-driven walkthrough — every step you'd type, every file you'd write. It's the right doc to read if you want to understand how the pieces fit, are debugging an unusual setup, or prefer not to delegate the bootstrap to an agent.

For the agent-driven flow, just run `giga setup` from your project directory — Claude Code opens with a baked-in prompt that does everything below for you. See [README.md](README.md). This doc is the reference of what `giga setup` is doing under the hood.

> **Single-host scope.** This walkthrough covers a swarm that runs entirely on one machine — today's default + most common case. For agents distributed across multiple machines on a tailnet, this is still the right starting doc (the multi-host swarm is a superset of single-host); once you've got a working single-host swarm, see [REMOTE_QUICKSTART.md](REMOTE_QUICKSTART.md) to add a second host, and [REMOTE_DESIGN.md](REMOTE_DESIGN.md) for the cross-host architecture.

## Background

`giga` is a CLI for running N parallel Claude Code (or other agent runtime) sessions that coordinate via append-only Markdown files. One terminal tab per agent; each tab opens in the agent's workdir with `claude` already running. Agents post to shared inbox files and a single `Monitor` per agent (`giga watch --as <slug>`) tails every channel that agent participates in. New messages become notifications. No MCP server, no message bus, no service to keep up — just files.

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

(Install giga first if you haven't — see [README.md § Install](README.md#install).)

The canonical location for swarm configs is `~/.giga/configs/<project>/`:

```sh
mkdir -p ~/.giga/configs/myproj/{agents,inbox,workdirs/design,workdirs/code,workdirs/test}
cd ~/.giga/configs/myproj
```

> Each agent gets its own **workdir** (where `CLAUDE.md` lives and `claude` launches) but they can share a **code_root** — the directory the agent actually edits. This keeps the launch context clean while letting multiple agents collaborate on one codebase.

### 2. Write `giga-harness.toml`

```toml
[project]
name = "myproj"

[paths]
wsl_inbox = "/home/me/.giga/configs/myproj/inbox"

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

Each template is your agent's CLAUDE.md — what they read on startup to know who they are. Minimal version of `agents/design.md`:

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
giga init       # creates inbox files + renders each agent's CLAUDE.md in their workdir
giga launch     # opens 3 terminal tabs, one per agent, each with claude already running
```

That's it. Each agent reads its CLAUDE.md, arms its `giga watch --as <slug>` watcher, posts a one-line "I'm online" intro on each of its channels, and waits for the other sides to talk. The first time `code` finishes a chunk, it posts on `code-design.md` and `design`'s watcher fires.

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
- Appends `[[channels]] code-review.md` and `design-review.md` (alphabetical filename convention).
- Appends `review` to `_broadcast.md` participants so they get all-hands announcements.
- Writes `agents/review.md` with a minimal stub template.
- Re-validates the config.

Use `--dry-run` first to preview. Use `--template <path>` to supply a fleshed-out CLAUDE.md instead of the stub.

Then bring `review` online without disrupting the running 3-agent tabs:

```sh
giga launch --only review --new-window
```

The new wt window pops up for `review`. The three existing tabs keep running. `review`'s single-Monitor watcher auto-discovers its two bilateral channels + the broadcast on its first tick. `design` and `code` are already on the same auto-discovery watcher (`giga watch --as <slug>`), so they pick up the new bilateral channels (`design-review.md`, `code-review.md`) on their next config reread (~15s) — no manual re-arming.

**Safe to run from inside an agent's session.** If `design` is the natural orchestrator, you can say "design, please add a review agent that..." and they can run `giga add-agent` themselves. Launch stays your call (window-layout intent is yours).

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
giga init        # re-renders agents/<slug>.md → workdir CLAUDE.md
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

For the broader operational guide (multi-host setups, stand-down → reactivation, Windows + WSL mixed ecosystems), see [QUICKSTART.md](QUICKSTART.md).

## Subcommands

| Command | What it does |
|---------|--------------|
| `giga setup` | One-command bootstrap. Launches Claude Code with a baked-in prompt that walks you through scaffolding a new swarm end-to-end. Run from your project directory. |
| `giga validate [config]` | TOML schema check + cross-reference. Also flags orphan channel files on disk that aren't enrolled in `[[channels]]`. No side effects. |
| `giga init [config]` | Creates inbox files + per-agent `CLAUDE.md` (idempotent — existing inbox files are kept). Registers the swarm in `~/.giga/swarms.toml`. |
| `giga add-agent --name X --workdir Y --role "..." [--code-root Z] --peer A [--peer B]` | Scaffold a new agent — appends `[[agents]]` + per-peer `[[channels]]`, adds to any `_broadcast.md` channel, writes `agents/<slug>.md`. `--code-root` lets the agent edit a shared codebase from an isolated workdir. `--dry-run` previews. Safe to run from within an agent's session. |
| `giga launch [config]` | One terminal per agent. `--terminal <mode>` picks the launcher: `auto` (default — wt > tmux > print), `mac-terminal` (one Terminal.app window per agent on macOS), `tmux`, `wt`, or `print`. `--only <a,b>` spawns just the named agents (non-disruptive add). `--new-window` forces a fresh wt window. Config resolution order: explicit `[config]` arg → ancestral `giga-harness.toml` walking up from cwd → `~/.giga/swarms.toml` registry lookup. |
| `giga sweep [config]` | Tabulate every channel's last message + open `WAITING ON` tags. |
| `giga post <channel> --as <agent> --subject ... [--body ... \| stdin] [--waiting-on <agent>]` | Append a properly-formatted message. Validates that the sender is a participant. `<channel>` accepts bare names (`pipeline-usage`) or the full `.md` form. |
| `giga watch --as <agent> [<channel>]` | Long-running watcher (use under Claude Code's `Monitor` tool). Without `<channel>`: config-aware multi-channel mode — auto-tracks every channel where `<agent>` is a participant and picks up new channels added later (~15s reread). With `<channel>`: legacy single-file mode. |

## Resuming after a reboot

`giga init` registers each swarm in `~/.giga/swarms.toml`, mapping `code_root` → config path. Any subsequent `giga launch` / `validate` / `sweep` / `watch` / `post` resolves the config via that registry — so you can `cd` to any directory under any agent's code root and run the command without `--config`:

```sh
cd ~/code/myproj
giga launch     # finds ~/.giga/configs/myproj/giga-harness.toml via the registry
```

The resolver also walks up from cwd looking for an ancestral `giga-harness.toml` *before* consulting the registry. This matters when you're already inside an agent's workdir (e.g. `~/.giga/configs/<swarm>/workdirs/<slug>/`) — the workdir tree isn't a `code_root`, so the registry lookup wouldn't find anything, but the ancestor walk lands on the config two levels up. Net effect: `giga watch --as <slug>` just works from a freshly-launched agent terminal.

If no swarm is registered for the current directory (or any ancestor) *and* no ancestral `giga-harness.toml` exists, giga prints a clear error pointing you to `giga setup`.

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

## Architecture

* **Agents** run wherever you want — WSL, Windows-native, remote SSH, doesn't matter. Each one is just a Claude Code (or other agent runtime) session in a terminal.
* **Channels** are plain text files in shared inbox directories. Both `side = "wsl"` and `side = "windows"` are supported on the same machine via the WSL/Windows interop boundary.
* **Watchers** are `giga watch --as <agent>` processes (one per agent), run under Claude Code's `Monitor` tool with `persistent: true`. One watcher per agent — it reads the config, tracks every channel that agent participates in, and rereads the config every ~15s so newly-added channels appear without restarting. Each new message becomes one stdout line (`inbox <channel>: ...`), which Claude Code treats as a notification.
* **Bench coordination** is just a convention layered on top — agents post `bench-request <slot>` and wait for `bench-clear <slot>` from the configured scheduler before doing heavy work.

There is deliberately no central service. If giga itself crashes, the agents keep talking.

License (MIT): see [LICENSE](LICENSE). Subcommand cheat sheet also lives in the [README](README.md#subcommands-at-a-glance).
