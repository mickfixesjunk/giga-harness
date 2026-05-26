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

### Step 2 — Run `giga setup` from your project directory

```sh
cd ~/code/my-project   # wherever your codebase lives
giga setup
```

That's it. `giga setup` launches Claude Code with a baked-in bootstrap prompt — no README copy/paste, no external docs to keep in sync. Claude asks you five questions:

1. **Project name** (kebab-case slug — becomes the config dir name)
2. **Which 2–4 agents** to spawn (typical: design + code + test, or with a review agent too)
3. **Where your code lives** (defaults to cwd)
4. **Topology** — single coordinator (recommended) vs. fully peer-to-peer
5. **Launcher** — `mac-terminal` (one Terminal.app window per agent on macOS), `tmux` (one session, N windows — works anywhere), `wt` (Windows Terminal), or `auto`

…then scaffolds the config, writes per-agent CLAUDE.md templates, runs `giga init` and `giga launch` for you. The agents come up, self-arm their inbox watchers, post hellos, and stand by for work.

### Resuming after a reboot

Just `cd` to your codebase and `giga launch`:

```sh
cd ~/code/my-project
giga launch
```

`giga init` registers each swarm in `~/.giga/swarms.toml`, mapping code roots to their config paths. `giga launch` (and `validate`/`sweep`/`watch`/`post`) auto-resolve via the registry — so any command, from anywhere under your code root, finds the right swarm.

### What just happened

- `giga` is on your `PATH`.
- Your swarm config lives at `~/.giga/configs/<project-name>/` — one TOML file describing your agents and their shared inbox channels, plus a workdir per agent, plus the inbox directory.
- A registry entry at `~/.giga/swarms.toml` maps your project's code root to this config.
- One terminal per agent (Terminal.app windows on macOS by default, tmux on Linux, Windows Terminal on WSL/Windows). Each agent's window title is the agent's slug; every reply they make is prefixed `[slug]` so you can always tell who's talking.

When you want to add another agent, ask one of them: "please add a `<role>` agent that does X — peer with `<existing>`. Use `giga add-agent`." They'll scaffold + validate; you run `giga launch --only <new-slug>` to bring up the new terminal.

## When you want more control

- **[MANUAL_SETUP.md](MANUAL_SETUP.md)** — full hand-written walkthrough. Read this if you want to understand every file, write the TOML yourself, or debug an unusual setup.
- **[QUICKSTART.md](QUICKSTART.md)** — lifecycle ops: adding, standing down, removing, reactivating agents.
- `giga --help`, `giga <subcommand> --help` — every subcommand has detailed help.

## Subcommands at a glance

| Command | What it does |
|---------|--------------|
| `giga setup` | One-command bootstrap. Launches Claude Code with a baked-in prompt that walks you through scaffolding a new swarm end-to-end. Run this from any project directory. |
| `giga validate [config]` | TOML schema + cross-reference check. Flags on-disk inbox files not enrolled in `[[channels]]`. No side effects. |
| `giga init [config]` | Creates inbox files + per-agent `CLAUDE.md` (idempotent). Registers the swarm in `~/.giga/swarms.toml`. |
| `giga add-agent --name X --workdir Y --role "..." [--code-root Z] --peer A [--peer B]` | Scaffold a new agent — `[[agents]]` + `[[channels]]` + broadcast participation + a stub template. `--code-root` lets the agent edit a shared codebase from an isolated workdir. `--dry-run` previews. |
| `giga launch [config]` | One terminal per agent. `--terminal <mode>` picks the launcher: `auto`, `mac-terminal` (Terminal.app), `tmux`, `wt`, or `print`. `--only <a,b>` spawns just the named agents (non-disruptive add). `--new-window` forces a fresh wt window. Auto-resolves config via `~/.giga/swarms.toml` registry if not in cwd. |
| `giga sweep [config]` | Tabulate every channel's last message + open `WAITING ON` tags. |
| `giga post <channel> --as <agent> --subject ...` | Append a properly-formatted message. |
| `giga watch --as <agent>` | Long-running watcher — auto-tracks every channel where the agent participates. Run under Claude Code's `Monitor` tool. |

## Key concepts

- **`workdir`** — the agent's isolated launch context. Their `CLAUDE.md` lives here; `claude` opens here. Default: `~/.giga/configs/<project>/workdirs/<agent>/`.
- **`code_root`** — *optional, separate from workdir.* The directory the agent actually edits in. Lets multiple agents share a single codebase while each has their own clean workdir. Set per-agent in TOML or via `giga add-agent --code-root <path>`.
- **Registry (`~/.giga/swarms.toml`)** — auto-maintained map of `code_root → config_path`. Lets `giga <command>` work from anywhere under your codebase, no `cd` required.
- **Window titles + reply prefixes** — every agent's terminal window is titled with their slug; every reply they post starts with `[slug]`. Hard to lose track of who's talking.

## License

MIT. See [LICENSE](LICENSE).
