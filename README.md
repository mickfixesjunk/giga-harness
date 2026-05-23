# giga-harness

Manual multi-agent coordination harness. One command, N terminals,
agents talking to each other through file-based inboxes.

```
giga launch
```

That's the whole pitch. You write one TOML file describing your
agents and their shared channels; `giga launch` opens one terminal
per agent (one Windows Terminal tab or one tmux window each), drops
each into its workdir, and starts `claude` so the agent reads its
`CLAUDE.md` and arms its watchers.

The agents then coordinate by appending to plain text files:

```
===
[design] T2.1 spec handed to testdesign — 2026-05-22T10:14:00Z
===

Scope agreed. Implementation up to engine.

WAITING ON: testdesign (spec walkthrough)
===
```

A watcher on the other end fires the moment that file grows. No MCP
server, no message bus, no service to keep up — just files.

## Why

Multi-agent coordination with Claude Code (or any agent runtime) keeps
reinventing the same primitives: an inbox, a watcher, a handoff
convention, a way to spawn N terminals on N projects. Doing it ad-hoc
means every project has its own scripts, every onboarding has its own
gotchas, and every "where are we stuck?" question needs a manual
review of half a dozen files.

`giga` is just the harness: terminal multiplexing, inbox scaffolding,
formatted message-posting, channel sweeping. Your project ships its
own config + CLAUDE.md templates in a separate repo (private if you
want), and `giga` glues them to your machine.

## Install

### From source (Rust toolchain)

```sh
cargo install --git https://github.com/mickfixesjunk/giga-harness
```

### Binary release

Pre-built binaries for Linux (x86_64), macOS (Apple Silicon), and
Windows (x86_64) attach to each release. One-line install:

```sh
curl -sSfL https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh | bash
```

The installer drops `giga` (or `giga.exe`) into `~/.local/bin`. Set
`GIGA_INSTALL_DIR=/some/other/dir` to override. On Windows native,
run the installer from WSL or Git Bash.

## Quick start

```sh
# 1. Clone or write a config repo.
git clone https://github.com/<your-org>/<your-configs>.git ~/giga-configs

# 2. Validate it.
cd ~/giga-configs/<your-project>
giga validate

# 3. Scaffold inbox files and per-agent CLAUDE.md.
giga init

# 4. Launch every agent's terminal.
giga launch
```

That's it. Every agent now has its workdir, its CLAUDE.md, and its
inbox watchers armed.

## Subcommands

| Command | What it does |
|---------|--------------|
| `giga validate <config>` | TOML schema check + cross-reference. No side effects. |
| `giga init <config>` | Creates inbox files + per-agent `CLAUDE.md` (idempotent). |
| `giga launch <config>` | One terminal per agent. Windows Terminal preferred, tmux fallback. |
| `giga sweep <config>` | Tabulate every channel's last message + open WAITING ON tags. |
| `giga post <channel> --as <agent> --subject ...` | Append a properly-formatted message. |
| `giga watch <channel> --as <agent>` | Long-running watcher (use under Claude Code's `Monitor` tool). |

## Config format

See [`examples/minimal/giga-harness.toml`](examples/minimal/giga-harness.toml)
for a 2-agent setup. The full schema:

```toml
[project]
name = "my-project"

[paths]
wsl_inbox     = "/home/me/inbox"     # required if any channel side = "wsl"
windows_inbox = "C:/Users/me/inbox"  # required if any channel side = "windows"

[[agents]]
name = "engine"
workdir = "/home/me/repos/engine"
role = "Implementation agent."
platform = "wsl"                 # "wsl" or "windows"
bench_scheduler = true           # at most one per host
claudemd_template = "agents/engine.md"  # optional; relative to config dir

[[channels]]
file = "engine-tester.md"
side = "wsl"                     # picks which inbox dir
participants = ["engine", "tester"]
purpose = "Test specs + results."

[bench_protocol]
scheduler = "engine"
slot_pool = "this-host"          # or "per-host" for multi-machine setups
```

## The convention

`giga` enforces one rule: every channel message ends with either

```
WAITING ON: <agent-name> (<what they need to do>)
```

or

```
(Informational, no response required.)
```

`giga sweep` reads these tags to tell you who owes whom. Ambiguous
closings — "I'll consider this agreed", "let me know if you have
concerns" — stall pipelines. The convention removes that whole class
of failure.

`giga post` writes the header + footer for you so agents can't forget.

## Architecture

* **Agents** run wherever you want — WSL, Windows-native, remote SSH, doesn't matter. Each one is just a Claude Code (or other agent runtime) session in a terminal.
* **Channels** are plain text files in shared inbox directories. Both `side = "wsl"` and `side = "windows"` are supported on the same machine via the WSL/Windows interop boundary.
* **Watchers** are `giga watch <channel> --as <agent>` processes, run under Claude Code's `Monitor` tool with `persistent: true`. They emit one stdout line per new message; Claude Code treats each line as a notification.
* **Bench coordination** is just a convention layered on top — agents post `bench-request <slot>` and wait for `bench-clear <slot>` from the configured scheduler before doing heavy work.

There is deliberately no central service. If giga itself crashes, the agents keep talking.

## License

MIT. See [LICENSE](LICENSE).
