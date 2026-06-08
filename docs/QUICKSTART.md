# Quickstart

Fast single-host getting-started guide for `giga` (v0.6.54). You just installed
the binary; this walks you from zero to a working 2-agent swarm that's actually
talking, with a tiny worked example you can copy-paste.

> **Even faster:** `giga setup` launches a Claude Code session with a baked-in
> bootstrap prompt that asks you a few questions and scaffolds + launches the
> swarm for you. Note: in that flow **giga itself writes nothing** — if you close
> the Claude session before it finishes, nothing is scaffolded. The steps below
> are the manual equivalent: useful for understanding what's going on, for
> scripting, or if you don't have `claude` on PATH.

> **Going multi-host?** This doc is single-host. To spread one swarm across two
> or more machines on a tailnet, see [REMOTE_QUICKSTART.md](REMOTE_QUICKSTART.md).
> The multi-host swarm is a strict superset — everything here still works.

> **Every command and flag:** [COMMAND_REFERENCE.md](COMMAND_REFERENCE.md).

## Mental model (read this first)

giga is a **manual multi-agent coordination harness**. There's no database, no
message bus, no LLM in the coordination loop — just plain text files and a
watcher per agent.

- A **swarm** is one project's set of agents plus the channels they talk on,
  described by a single `giga-harness.toml`.
- An **agent** runs an interactive CLI (Claude Code by default) in its own
  terminal tab, in its own **workdir**, guided by an `AGENTS.md` file giga
  generates.
- Agents communicate **only** by appending messages to **channel files** —
  Markdown files in a shared inbox directory. A **bilateral** channel connects
  two agents; its filename is the two slugs sorted alphabetically (`alice-bob.md`).
- Each agent runs a long-lived **watcher** (`giga watch`) that tails the channels
  it participates in and surfaces new messages into its session.
- Every message ends with one of two footers: `WAITING ON: <agent>` (a reply is
  owed) or `(Informational, no response required.)`. That footer is what keeps
  the pipeline from stalling.

A channel message looks like this:

```
===
[alice] spec for the parser — 2026-06-08T14:30:00Z
===

Scope agreed: CSV import only, no edge-case fanout this phase.

WAITING ON: bob (acknowledge + estimate)
===
```

## 1. Write the config

A swarm is one TOML file plus a folder of agent templates. A good home for it is
`~/.giga/configs/<project>/`. Scaffold the directory:

```sh
mkdir -p ~/.giga/configs/myproject/agents
cd ~/.giga/configs/myproject
```

Write `giga-harness.toml`. Minimal 2-agent example (alice implements, bob
reviews; they share one bilateral channel):

```toml
[project]
name = "myproject"

[paths]
wsl_inbox = "/home/me/.giga/configs/myproject/inbox"   # required if any channel side = "wsl"

[[agents]]
name = "alice"
workdir = "/home/me/.giga/configs/myproject/workdirs/alice"
code_root = "/home/me/code/myproject"   # optional — where alice edits code
role = "Implementation."
platform = "wsl"
claudemd_template = "agents/alice.md"

[[agents]]
name = "bob"
workdir = "/home/me/.giga/configs/myproject/workdirs/bob"
code_root = "/home/me/code/myproject"
role = "Review."
platform = "wsl"
claudemd_template = "agents/bob.md"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
purpose = "Implementation ↔ review handoffs."
```

Write `agents/alice.md` and `agents/bob.md`. Each one tells its agent who it is,
how to arm its watcher, and the message convention. Here's `agents/alice.md`:

````markdown
# alice agent

You are the **implementation** agent for myproject. Always prefix every reply
with `[alice]`.

## Session Start

1. Post an intro on each of your channels with
   `giga post <channel> --as alice --subject "online" --body "..."`.
2. Arm the Monitor below.
3. Standby.

## Channels you watch

```
Monitor(persistent: true, command: "giga watch --as alice")
```

One watcher auto-discovers every channel where you participate (from
`giga-harness.toml`) and re-reads the config periodically, so new channels are
picked up without re-arming.

## Convention

Close every channel message with `WAITING ON: <agent> (<what>)` or
`(Informational, no response required.)`.
````

`agents/bob.md` is the same with `bob`/`Review.` swapped in.

> giga writes a single **`AGENTS.md`** per workdir (universal across runtimes) —
> not `CLAUDE.md`. It's re-rendered from `claudemd_template` on every `init` /
> `launch`, so edit the source template, never the generated workdir copy.

## 2. Validate, init, launch

```sh
giga validate          # read-only config sanity check; no filesystem changes
giga init              # scaffold inbox files + render each agent's AGENTS.md
giga launch            # spawn one terminal per agent (runs init first)
```

`giga validate` prints `ok: <path> (<project>) — 2 agents, 1 channels` and, per
channel, whether the file exists yet or will be created by `init`.

`giga launch` auto-detects your terminal: Windows Terminal (`wt.exe`) if present,
else `tmux`, else `print`. Common overrides:

```sh
giga launch --terminal mac-terminal   # macOS: one native Terminal.app window per agent
giga launch --dry-run                 # print the launch plan, spawn nothing
giga launch --skip-init               # don't re-run init first

# 8+ agents? spread the CLI starts so N `claude` first turns don't hit
# Anthropic's TPM limit at once:
giga launch --stagger-per-agent-seconds 10
```

Each terminal opens in the agent's workdir with `claude` already running. The
agent reads its `AGENTS.md`, arms its watcher, posts its intro, and waits. Every
reply is prefixed with `[slug]` so you always know who's talking.

> **Resuming after a reboot:** `giga init` registered your swarm in
> `~/.giga/swarms.toml` against its `code_root`. Just `cd ~/code/myproject &&
> giga launch` — giga resolves the right config from the registry.

## What appears after `init`

`giga init` is a deterministic, idempotent scaffolder. After running it you'll see:

- **Channel files** in your inbox dir (`inbox/alice-bob.md`), each with a
  convention header. `init` reports `[new]` or `[keep]`.
- **`AGENTS.md`** in each agent's workdir — always overwritten (`[gen]`).
- **`HANDOVER.md`** in each workdir on first init only (`[hand]`/`[keep]`), seeded
  from `agents/<slug>.handover.md` if present.
- A **`giga-harness.toml` symlink** in each workdir on Unix (`[link]`).
- A **swarm entry** upserted into `~/.giga/swarms.toml` (`[reg]`) — `init` is the
  only writer of this registry.
- Claude Code **per-folder trust** marked for each workdir + `code_root` so agents
  don't prompt on first launch (skip with `--no-trust`).

It prints `ginit OK` when done.

## 3. A worked example: alice waits on bob

Now watch a real exchange flow through the channel. Open two terminals (your two
agent tabs), and use a third for operator commands.

**alice posts a question and blocks on bob.** From alice's session (or any
terminal — `post` just appends to the file):

```sh
giga post alice-bob.md --as alice \
  --subject "spec for the parser" \
  --body "CSV import only this phase. Acknowledge + give me an estimate." \
  --waiting-on bob --needs "estimate"
```

This appends a properly-formatted block to `inbox/alice-bob.md` and prints
`posted to <path> (<n> bytes)`. The footer becomes
`WAITING ON: bob (estimate)` — bob now owes a reply, and alice waits silently
(blocked agents don't re-ping).

**bob's watcher surfaces it.** bob's `giga watch --as bob` Monitor tails
`alice-bob.md`, sees the new message, and emits a line into bob's session:

```
inbox alice-bob.md: [alice] spec for the parser — 2026-06-08T14:30:00Z
```

bob reads it and responds.

**Check who owes what at any time.** From your operator terminal:

```sh
giga sweep
```

`giga sweep` tabulates every channel's last message and its open `WAITING ON`
tag, with a trailing `N channels with open WAITING ON tag`:

```
channel       last_from   subject              waiting_on
alice-bob.md  alice       spec for the parser  bob

1 channels with open WAITING ON tag
```

Narrow it to a single agent's debts with `giga sweep --owed-by bob`.

**bob clears the wait.** bob replies on the channel, pivoting the wait back to
alice (or closing it as informational):

```sh
# bob owes alice an answer, then waits for alice's go-ahead:
giga post alice-bob.md --as bob \
  --subject "estimate" \
  --body "~2 hours. Starting now unless you object." \
  --waiting-on alice

# or close it out with no reply owed:
giga post alice-bob.md --as bob \
  --subject "ack" \
  --body "Got it, on it."
# (no --waiting-on → footer is "(Informational, no response required.)")
```

alice's watcher surfaces bob's reply, and `giga sweep` now shows the wait flipped
to alice (or `informational` if bob closed it). That's the whole loop:
post → watch surfaces it → reply → sweep confirms.

> **Why the footer matters.** A `WAITING ON: <me>` an agent misses (e.g. it
> compacted its context) would stall forever — so `giga watch` also runs **stale-wait
> detection**: it re-derives unresolved waits from full channel history and
> re-surfaces any older than the threshold (`⏰ STALE WAIT`). Pure file I/O, no LLM
> cost. There's no separate subcommand — it's built into `watch`.

## 4. Watching, in detail

`giga watch` is meant to run under Claude Code's **Monitor** tool with
`persistent: true` (as in the templates above) — that's how its stdout reaches
the conversation. A `giga watch` launched from a Bash tool stays alive but its
output never reaches the agent, so it gets zero notifications.

```sh
giga watch --as alice               # multi-channel: every channel where alice participates
giga watch alice-bob.md --as alice  # single-file: just this one channel
```

Multi-channel mode (no channel argument) is the normal case: it auto-discovers
every channel alice is on and re-reads the config every ~15s, so channels added
later are picked up without restarting the watcher. The first time an agent
watches a channel it replays the full history as catch-up; later sessions resume
from a stored cursor.

## Broadcast channels (when you grow past two agents)

A channel whose filename starts with `_` (e.g. `_broadcast.md`) is a **broadcast**
channel with many participants. `giga post` gains two addressing flags there:

```sh
# Reach a subset — only the listed agents fire a notification:
giga post _broadcast.md --as alice --to bob,carol --subject "..." --body "..."

# Informational, zero LLM cost — receivers archive instead of notifying:
giga post _broadcast.md --as alice --fyi --subject "alice online" --body "..."
```

`--to` and `--fyi` are mutually exclusive and are no-ops on bilateral channels.

## Cheat sheet

| Goal | Command |
|------|---------|
| Cold start a swarm | `giga validate && giga init && giga launch` |
| Cold start 8+ agents (avoid TPM storm) | `giga launch --stagger-per-agent-seconds 10` |
| Validate a config edit | `giga validate` |
| See open `WAITING ON` tags | `giga sweep` |
| See what one agent owes | `giga sweep --owed-by <agent>` |
| Post a message + block on a reply | `giga post <channel> --as <a> --subject "..." --body "..." --waiting-on <b>` |
| Post an informational message | `giga post <channel> --as <a> --subject "..." --body "..."` |
| Multi-channel watcher (normal) | `giga watch --as <agent>` |
| Single-channel watcher | `giga watch <channel> --as <agent>` |

## Next steps

- Adding, removing, or standing an agent down; `add-agent`, `add-channel`,
  `takeover`, `set-swarm-boss`, the `giga ui` dashboard, and every flag:
  [COMMAND_REFERENCE.md](COMMAND_REFERENCE.md).
- Hand-rolling a swarm step by step (the full TOML schema):
  [MANUAL_SETUP.md](MANUAL_SETUP.md).
- Spreading the swarm across two machines on a tailnet:
  [REMOTE_QUICKSTART.md](REMOTE_QUICKSTART.md).
- Operating a swarm from inside a Claude session:
  [CLAUDE_OPERATOR.md](../templates/CLAUDE_OPERATOR.md) (or run `giga claude-operator`).
- Back to the [README](../README.md).
