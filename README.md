# giga-harness

[![ci](https://github.com/mickfixesjunk/giga-harness/actions/workflows/ci.yml/badge.svg)](https://github.com/mickfixesjunk/giga-harness/actions/workflows/ci.yml)

A coordination harness for running N parallel AI coding agents (Claude Code, Codex, etc.) that talk to each other through file-based inboxes. One terminal tab per agent; each agent reads its own `CLAUDE.md`, watches a shared inbox, and posts back when it has something to say.

```
===
[design] T2.1 spec ready — 2026-05-22T10:14:00Z
===

Scope agreed: import-from-CSV, no edge-case fanout this phase.

WAITING ON: code (acknowledge + estimate)
===
```

No message bus, no MCP server, no service to keep up — just plain text files in a shared directory + a watcher per agent.

## Onboarding (you'll do this once, ~2 minutes)

### Step 1 — Install

**Linux / macOS / WSL:**

```sh
curl -sSfL https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh | bash
```

**Windows (PowerShell):**

```powershell
irm https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.ps1 | iex
```

Confirm:

```sh
giga --version    # should be 0.1.11 or newer
```

### Step 2 — Paste this prompt into Claude Code (or Codex, or whatever you use)

Open your AI coding agent in a fresh terminal session and paste:

```
I want to use giga-harness to coordinate a small team of AI agents on a project.

Please bootstrap it:

1. Confirm giga is installed and ≥ 0.1.11: `giga --version`.
2. Ask me 4 questions (one at a time, or all at once — your call):
   - Project name (kebab-case, e.g. "my-saas-side-project").
   - What 2-4 agents I want — typically a mix like: design (scopes
     features), code (implements), test (verifies), review (audits).
     I'll give you a slug + one-line role for each.
   - Where my project code lives (absolute path). Agent workdirs default
     to subdirectories of this unless I say otherwise.
   - Whether I want any of them to peer with each other directly, or
     route everything through a single coordinator (design is typical).
3. Create a project config directory at ~/giga-configs/<project-name>/.
4. Write giga-harness.toml: one `[[agents]]` block per agent + one
   bilateral `[[channels]]` block per peering + a `_broadcast.md`
   channel with all agents as participants.
5. Use `giga add-agent --help` and `giga validate --help` to see the
   command surface. Run `giga init` (creates the inbox files + each
   agent's CLAUDE.md) and then `giga launch` (opens one terminal tab
   per agent, drops each into `claude` with their CLAUDE.md loaded).
6. Tell me what just happened and what the agents are doing now.

If anything's unclear, read https://github.com/mickfixesjunk/giga-harness/blob/main/MANUAL_SETUP.md
for the full conventions. If giga is too old, the upgrade is the same
one-line install command from the README.
```

That's it. The agent walks you through the bootstrap, scaffolds the config, and launches your swarm. After this you can ask the same agent — or any of the spawned ones — to add more agents, tweak roles, or stand one down ("can you add a `docs` agent that owns README + API docs and routes through design?"). They'll know how because they read the same protocol.

### What just happened

- `giga` is now on your `PATH`.
- A config repo lives at `~/giga-configs/<your-project>/` with one TOML file describing your agents and their shared inbox channels.
- A terminal multiplexer (Windows Terminal on WSL, tmux on Linux) is running one tab per agent, each tab in the agent's workdir with `claude` listening on its inbox.
- Agents are coordinating through plain Markdown files in `~/giga-configs/<your-project>/inbox/`.

When you want to add another agent, ask one of them: "please add a `<role>` agent that does X — peer with `<existing>`. Use `giga add-agent`." They'll scaffold + validate; you run `giga launch --only <new-slug> --new-window` to bring up the tab.

## When you want more control

- **[MANUAL_SETUP.md](MANUAL_SETUP.md)** — full hand-written walkthrough. Read this if you want to understand every file, write the TOML yourself, or debug an unusual setup.
- **[QUICKSTART.md](QUICKSTART.md)** — lifecycle ops: adding, standing down, removing, reactivating agents.
- `giga --help`, `giga <subcommand> --help` — every subcommand has detailed help.

## Subcommands at a glance

| Command | What it does |
|---------|--------------|
| `giga validate [config]` | TOML schema + cross-reference check. Flags on-disk inbox files not enrolled in `[[channels]]`. No side effects. |
| `giga init [config]` | Creates inbox files + per-agent `CLAUDE.md` (idempotent). |
| `giga add-agent --name X --workdir Y --role "..." --peer A [--peer B]` | Scaffold a new agent — `[[agents]]` + `[[channels]]` + broadcast participation + a stub template. `--dry-run` previews. |
| `giga launch [config]` | One terminal per agent. `--only <a,b>` spawns just the named agents (non-disruptive add). `--new-window` (Windows Terminal only) forces a fresh window. |
| `giga sweep [config]` | Tabulate every channel's last message + open `WAITING ON` tags. |
| `giga post <channel> --as <agent> --subject ...` | Append a properly-formatted message. |
| `giga watch --as <agent>` | Long-running watcher — auto-tracks every channel where the agent participates. Run under Claude Code's `Monitor` tool. |

## License

MIT. See [LICENSE](LICENSE).
