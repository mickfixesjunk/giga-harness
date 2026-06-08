# giga-harness

[![ci](https://github.com/mickfixesjunk/giga-harness/actions/workflows/ci.yml/badge.svg)](https://github.com/mickfixesjunk/giga-harness/actions/workflows/ci.yml)

**Manual multi-agent coordination harness.** A small Rust CLI (`giga`) that spawns and wires up N parallel AI coding agents (Claude Code, Codex, Antigravity) which coordinate by **appending messages to shared Markdown files**. No database, no message bus, no MCP server, no LLM in the coordination loop — just plain text files in a shared directory plus a watcher per agent.

One terminal tab per agent; each agent runs in its own workdir guided by a giga-generated `AGENTS.md`, watches a shared inbox, and posts back when it has something to say.

```
===
[design] T2.1 spec ready — 2026-05-22T10:14:00Z
===

Scope agreed: import-from-CSV, no edge-case fanout this phase.

WAITING ON: code (acknowledge + estimate)
===
```

Agents can run on **one machine** (the default) or across **multiple machines** via a pluggable transport. See [§ Multi-host swarms](#multi-host-swarms-cross-host-channels).

## Install

**Linux / macOS / WSL:**

```sh
curl -sSfL https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh | bash
```

**Windows (PowerShell):**

```powershell
irm https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.ps1 | iex
```

**From source** (puts `giga` in `~/.cargo/bin`):

```sh
cargo install --path .
```

Confirm:

```sh
giga --version    # should be 0.6.54 or newer
```

## Quickstart (~2 minutes)

### Step 1 — Run `giga setup` from your project directory

```sh
cd ~/code/my-project   # wherever your codebase lives
giga setup
```

That's it. `giga setup` launches Claude Code with a baked-in bootstrap prompt — no README copy/paste, no external docs to keep in sync. The spawned agent asks you a handful of questions (about six):

1. **Project name** (kebab-case slug — becomes the config dir name)
2. **Which 2–4 agents** to spawn (typical: design + code + test, or with a review agent too)
3. **Where your code lives** (defaults to cwd)
4. **Topology** — single coordinator (recommended) vs. fully peer-to-peer
5. **Launcher** — `mac-terminal` (one Terminal.app window per agent on macOS), `tmux` (one session, N windows — works anywhere), `wt` (Windows Terminal), or `auto`
6. **swarm_boss** — which agent, if any, hosts the cross-host sync/merger daemons (only relevant once you go multi-host)

…then scaffolds the config, writes a per-agent `AGENTS.md`, runs `giga init` and `giga launch` for you. The agents come up, self-arm their inbox watchers, post hellos, and stand by for work.

> Note: `giga setup` does all the scaffolding inside the spawned Claude session — `giga` itself writes nothing in the guided path. If you close the session before it finishes, nothing is created.

### Step 2 — Resuming after a reboot

Just `cd` to your codebase and `giga launch`:

```sh
cd ~/code/my-project
giga launch
```

`giga init` registers each swarm in `~/.giga/swarms.toml`, mapping code roots to their config paths. `giga launch` (and `validate` / `sweep` / `watch` / `post`) auto-resolve via the registry — so any command, from anywhere under your code root, finds the right swarm. (Run `giga validate` first for a no-side-effect config check before launching; run `giga init` from the swarm dir, as it uses the config path literally rather than resolving via the registry.)

### What just happened

- `giga` is on your `PATH`.
- Your swarm config lives at `~/.giga/configs/<project-name>/` — one TOML file describing your agents and their shared inbox channels, plus a workdir per agent, plus the inbox directory.
- A registry entry at `~/.giga/swarms.toml` maps your project's code root to this config.
- One terminal per agent (Terminal.app windows on macOS by default, tmux on Linux, Windows Terminal on WSL/Windows). Each agent's window title is the agent's slug; every reply they make is prefixed `[slug]` so you can always tell who's talking.

When you want to add another agent, ask one of them: "please add a `<role>` agent that does X — peer with `<existing>`. Use `giga add-agent`." They'll scaffold + validate; you run `giga launch --only <new-slug>` to bring up the new terminal.

## When you want more control

- **[docs/MANUAL_SETUP.md](docs/MANUAL_SETUP.md)** — full hand-written walkthrough. Read this if you want to understand every file, write the TOML yourself, or debug an unusual setup.
- **[docs/QUICKSTART.md](docs/QUICKSTART.md)** — single-host getting-started walkthrough: `setup` → `init` → `launch` → `post`/`sweep`/`watch` with a worked two-agent example.
- **[docs/COMMAND_REFERENCE.md](docs/COMMAND_REFERENCE.md)** — every subcommand with examples, flag breakdowns, and a quick-lookup-by-goal table.
- **[docs/REMOTE_QUICKSTART.md](docs/REMOTE_QUICKSTART.md)** — operator runbook for adding a second host to a swarm (both transports), the `[[hosts]]` schema deep dive, and troubleshooting.
- **[templates/CLAUDE_OPERATOR.md](templates/CLAUDE_OPERATOR.md)** — the operator command surface, baked into the binary and printed by `giga claude-operator`.
- `giga --help`, `giga <subcommand> --help` — every subcommand has detailed help.

## Subcommands at a glance

### Single-host (the default)

| Command | What it does |
|---------|--------------|
| `giga setup` | One-command bootstrap. Launches Claude Code with a baked-in prompt that walks you through scaffolding a new swarm end-to-end. Run this from any project directory. |
| `giga validate [config]` | TOML schema + cross-reference check. Flags on-disk inbox files not enrolled in `[[channels]]`. No side effects. |
| `giga init [config]` | Creates inbox files + a per-agent `AGENTS.md` (idempotent). Registers the swarm in `~/.giga/swarms.toml`. Host-aware: in a multi-host swarm, only scaffolds agents whose `host` matches `this_host`. Uses the config path literally — run it from the swarm dir. |
| `giga add-agent --name X --workdir Y --role "..." [--code-root Z] [--host H] --peer A [--peer B]` | Scaffold a new agent — `[[agents]]` + `[[channels]]` + broadcast participation + a stub template. `--code-root` lets the agent edit a shared codebase from an isolated workdir. `--host` puts the agent on a peer host (auto-bootstraps the peer + scaffolds the workdir there). `--dry-run` previews. |
| `giga add-channel --participants A,B [--file ...]` | Append a new bilateral channel to the canonical TOML. v1 supports 2 participants only; auto-derives `<a>-<b>.md` filename. |
| `giga launch [config] [--host H]` | One terminal per agent. `--terminal <mode>` picks the launcher: `auto`, `mac-terminal` (Terminal.app), `tmux`, `wt`, or `print`. `--only <a,b>` spawns just the named agents (non-disruptive add). `--new-window` forces a fresh wt window. `--stagger-per-agent-seconds N` paces per-agent start-up (use 5–15s for 10+ agent swarms to avoid TPM-limit storms from N simultaneous `claude` first turns). `--host H` runs launch on a peer over SSH. `--ui` also spawns a `giga ui` dashboard pane. |
| `giga sweep [config] [--host H]` | Tabulate every channel's last message + open `WAITING ON` tags. `--owed-by <agent>` filters to channels where that agent is the one being waited on. `--host H` runs sweep on the peer (output streams back). |
| `giga post <channel> --as <agent> --subject ...` | Append a properly-formatted message. `<channel>` accepts the bare name or `.md`-suffixed form (positional or via `--channel`). `--waiting-on <agent>` tags a reply as owed; omit for informational. Cross-host channels auto-route to per-host slice files (`<channel>.<this_host>.md`). |
| `giga watch --as <agent>` | Long-running watcher — auto-tracks every channel where the agent participates. Run under Claude Code's `Monitor` tool (a Bash-launched watcher's stdout never reaches the session). |
| `giga takeover [--as <slug>] [--to <runtime>]` | Flip an agent's runtime in place (claude/codex/agy) — re-renders `AGENTS.md`, appends a HANDOVER.md block, prints a one-shot prompt. |
| `giga switch --runtime claude [<account>]` | Multi-account credential manager. See [§ Multi-account switching](#multi-account-switching). |
| `giga ui [--bind <addr>] [--port <n>]` | Browser dashboard for every registered swarm on this machine (default `127.0.0.1:7878`). |
| `giga upgrade [--bare] [--dry-run] ...` | Install the latest `giga` binary (and, in a swarm, on every peer) then broadcast a watcher re-arm. |
| `giga claude-operator` | Operator help for Claude: at a TTY it drops into a Claude session preloaded with the giga command surface; piped, it prints the doc. |

For the full command surface (22 subcommands with examples, flag breakdowns, and a quick-lookup-by-goal table), see **[docs/COMMAND_REFERENCE.md](docs/COMMAND_REFERENCE.md)**. The table above covers the everyday single-host commands; the reference covers all of them including `giga teleport`, `giga set-swarm-boss`, `giga hosts`, `giga remote`, the daemons (`sync`, `merger`, `watch`), the bootstrap helpers (`giga setup --remote-node`, `giga add-host`), and `giga codex-channel`.

### Multi-host (cross-host channels)

| Command | What it does |
|---------|--------------|
| `giga setup --remote-node` | Bootstrap a bare WSL host as a swarm peer: installs Tailscale + rsync, runs `tailscale up` (interactive auth), enables Tailscale SSH, creates the inbox dir. Run on the new host first; then `add-host` from the operator side. (`--transport git` installs git + rsync and smoke-tests a state repo instead.) |
| `giga add-host --name H --tailnet-hostname FQDN [--ssh-user U] [--remote-config-dir P] [--remote-inbox-dir P]` | Append a `[[hosts]]` entry to the canonical TOML and (by default) auto-bootstrap the new peer: mkdir + rsync swarm dir + ensure the peer has a `this_host.toml`. `--no-bootstrap` opts out. |
| `giga remote --host H -- <subcommand>` | SSH passthrough primitive — runs any giga subcommand on the peer over Tailscale SSH, streaming stdout/stderr back. The `--host H` flags on `launch` / `sweep` / `add-agent`'s auto-launch are sugar over this. Put trailing args after `--`. Only the `rsync+tailscale` transport supports remote exec. |
| `giga sync [--once] [--dry-run] [--quiet]` | Long-running daemon — every ~3s, rsync (or git-push) the canonical TOML + own slice files to each peer. Re-reads the config every ~15s so post-launch `add-agent` / `add-channel` is picked up automatically. |
| `giga merger [--once] [--quiet]` | Long-running daemon — polls all `<channel>.<host>.md` slice files and appends new peer bytes to the watched `<channel>.md`. Auto-spawned by `giga launch` on cross-host swarms. |
| `giga post <channel> [--to A,B] [--fyi] ...` | Append a message. For broadcast channels (`_*.md`): `--to` synthesizes an `[ack: A, B]` subject prefix so only named agents wake; `--fyi` synthesizes `[fyi]` (zero LLM cost; receivers archive instead of firing). Mutually exclusive. |
| `giga watch --as <agent> [--stagger-seconds N \| --no-stagger] [--agy \| --codex]` | Long-running watcher. Posts on `_*.md` channels stagger per-agent by `slot × stagger_seconds` (default 30s; override via `[broadcast].stagger_seconds` in TOML or `--stagger-seconds N` / `--no-stagger` per invocation). `--agy` / `--codex` switch the delivery mode for Antigravity / Codex runtimes. |
| `giga teleport <agent> --to <host> [--from <host>] [--keep-running] [--dry-run]` | Move an agent from one host to another. Updates TOML, rsyncs workdir over tailnet SSH, prepends a teleport banner to HANDOVER.md, kills the source pane gracefully, launches on the target. |
| `giga set-swarm-boss <slug> [--unset]` | Promote (or demote) the agent that runs the per-host `sync` + `merger` daemons via Monitors in its `AGENTS.md`. At most one per host; must be `platform=wsl`. |
| `giga hosts [--available]` | Read-only topology view — which agents live on each host and whether `this_host` matches. `--available` lists tailnet members not yet registered. |

## Multi-account switching

When you hit a rate-limit cap and want to migrate the whole swarm to an overflow account (different Anthropic plan, billing identity, whatever) without losing per-agent transcripts.

**One-time setup** — name your current credentials:

```sh
giga switch --runtime claude --setup primary
```

This creates `~/.claude-accounts/primary.json` from your existing `~/.claude/.credentials.json` and records `primary` as active.

**Add an overflow account:**

```sh
giga switch --runtime claude --add overflow
giga switch --runtime claude overflow      # make it active (empty slot, so /login required)
claude                                     # opens; go through /login as the overflow identity
giga switch --runtime claude primary       # switch back; the new tokens are saved to overflow.json
```

**Day-to-day switching:**

```sh
giga switch --runtime claude overflow      # flip the active credentials
pkill -f '^claude$'                        # or close the agent tabs
giga launch                                # tabs re-spawn as `claude -c`, resuming on the new account
```

Running `claude` processes keep their old auth in memory — they need to be killed and re-launched. `claude -c` (which `giga launch` uses) resumes each agent's transcript per-workdir, so conversation history and in-flight work survive the switch unchanged. Only the billing account changes.

**How it works.** `~/.claude/.credentials.json` is the real, live credentials file claude reads + refreshes. `~/.claude-accounts/<name>.json` is a snapshot per account, plus `.active` marks which one is currently live. Switching copies the live file back to its snapshot (preserving any in-place OAuth refreshes), then copies the target snapshot into the live file. No symlinks — `/login` and silent token refreshes use write-temp-then-rename, which destroys symlinks, so we use plain file copies instead. `mcpOAuth` tokens travel with the account.

**Limitations.** Claude-only today (`--runtime claude`). Linux/macOS/WSL only — Windows-native isn't wired up yet. Whole-swarm switch, not per-agent.

## Multi-host swarms (cross-host channels)

A giga swarm can span multiple physical machines. Agents on different hosts participate in the same channels as if they were local; the single-host model stays the fast-path for all-local channels.

**How it works (one paragraph).** Each cross-host channel has per-host slice files `<channel>.<host>.md` next to the merged `<channel>.md`. When an agent posts on a cross-host channel, `giga post` **dual-writes** the same frame to its host's slice (for the sync daemon to ship to peers) AND to the merged file (so the local watcher sees it immediately — independent of any daemon's liveness). A local `giga sync` daemon ships each host's own slices to peers; a local `giga merger` daemon appends incoming PEER slice bytes to the merged file. Channels with all participants on `this_host` skip the slice path entirely. Reception is push-only and symmetric.

### Transports

A swarm picks **one** transport for its lifetime; all hosts must use the same one.

| Transport | Connectivity | Latency | Setup | Remote exec (`--host`) |
|---|---|---|---|---|
| `rsync+tailscale` | mutual tailnet membership | ~5s | install Tailscale on each peer, click the auth URL | **yes** |
| `git` | any git host (GitHub/GitLab/self-hosted) | ~10s | create a private state repo; each peer clones it | no — run giga directly on the peer |
| `local` | (single-host only) | n/a | nothing | n/a |

Only `rsync+tailscale` supports remote exec, so the `--host` flags (`launch --host`, `sweep --host`, `giga remote`) work only under it. Under `git` (or `local`) those commands error cleanly and tell you to run giga directly on the peer.

### Bootstrap a new tailnet peer — 2 shots (rsync+tailscale)

Install `giga` on both hosts (see [Install](#install)).

**Shot 1 — on the NEW host** (interactive: prints a Tailscale auth URL):

```sh
giga setup --remote-node                                # defaults to --transport rsync+tailscale
```

Installs Tailscale + rsync, runs `tailscale up`, enables Tailscale SSH, creates `~/projects/inbox`. ~5 min. It prints this host's tailnet FQDN for the next step.

**Shot 2 — on the OPERATOR host:**

```sh
giga add-host --name wsl-b \
              --tailnet-hostname wsl-b.tail0000.ts.net \
              --ssh-user neo \
              --remote-config-dir /home/neo/.giga/configs/<swarm>
giga add-agent --host wsl-b --name <slug> --peer <existing> --role "..." \
               --workdir /home/neo/.giga/configs/<swarm>/workdirs/<slug>
giga launch --host wsl-b --only <slug>
```

Post-to-fire latency: ~3–10 seconds.

> For the **git transport** bootstrap, the `swarm_boss` setup, the full `[[hosts]]` schema, and troubleshooting, see **[docs/REMOTE_QUICKSTART.md](docs/REMOTE_QUICKSTART.md)**.

### Multi-runtime (Claude / Codex / Antigravity)

Swarms can mix agent runtimes on the same channels. Set the project-wide default in TOML, or override per-agent:

```toml
[project]
runtime = "claude"  # or "codex", "agy"

[[agents]]
name = "research"
runtime = "agy"
```

| Runtime | Launch command | Watcher mode | Pane count |
|---|---|---|---|
| `claude` (default) | `claude -c --model <m> <intro>` | `giga watch --as <agent>` (Monitor tool inside the session) | 1 |
| `agy` (Antigravity) | `agy -i <intro>` | `giga watch --agy` (background task; exits on `WAITING ON: <me>`) | 1 |
| `codex` (Codex CLI) | `codex` (intro arrives via inbox envelope) | `giga watch --codex` (separate `<agent>-bridge` pane) | **2** |

Every agent gets a single universal `AGENTS.md` (since v0.6.0; never a per-runtime `CLAUDE.md`). The Session Start section adapts per runtime — Monitor instructions for Claude, background-task instructions for AGY, bridge-pane explanation for Codex. `AGENTS.md` is re-rendered on every `init`/`launch`, so persistent edits must go to the source `claudemd_template`, not the workdir copy.

For codex agents specifically, `giga init` scaffolds `<workdir>/codex-channel/{inbox,outbox,processed}` and `giga launch` spawns two panes: `<agent>-cli` (the codex CLI with `CODEX_CHANNEL_DIR` set) and `<agent>-bridge` (running `giga watch --codex`).

### Current limitations

- Only WSL/Linux peers in v1; Windows-native peers need WSL.
- Push topology is O(N²) connections per tick — fine up to ~5 hosts; hub-and-spoke for more.
- Transports are `rsync+tailscale` or `git`; remote exec (`--host`) is tailscale-only. (An S3/R2 cloud-storage transport is a future follow-up.)

## Key concepts

- **Channels** — Markdown files in a shared inbox dir. A message is an append-only block whose header is `[<sender>] <subject> — <UTC-ISO8601>` and whose footer is either `WAITING ON: <agent>` (a reply is owed) or `(Informational, no response required.)`. Bilateral channels join exactly two agents (`<a>-<b>.md`); broadcast channels start with `_` (e.g. `_broadcast.md`).
- **`workdir`** — the agent's isolated launch context. Its `AGENTS.md` lives here; the CLI opens here. Default: `~/.giga/configs/<project>/workdirs/<agent>/`.
- **`code_root`** — *optional, separate from workdir.* The directory the agent actually edits in. Lets multiple agents share a single codebase while each has their own clean workdir. Set per-agent in TOML or via `giga add-agent --code-root <path>`.
- **Registry (`~/.giga/swarms.toml`)** — auto-maintained map of `code_root → config_path`, written only by `giga init`. Lets `giga <command>` work from anywhere under your codebase, no `cd` required.
- **Window titles + reply prefixes** — every agent's terminal window is titled with its slug; every reply starts with `[slug]`. Hard to lose track of who's talking.
- **`[[hosts]]` + `this_host`** *(multi-host only)* — `[[hosts]]` enumerates the physical machines in the swarm; each agent's `host` field names which one it runs on. Each host has a one-line identity file next to its canonical config (`this_host.local.toml` preferred; `this_host.toml` is the legacy fallback). Absent for all-local swarms.
- **Slice files** *(multi-host only)* — `<channel>.<host>.md` is the single-writer wire format. Each host appends only to its own slice; the local merger reads everyone's slices and appends to the merged `<channel>.md` that the watcher tails. Append-only by construction.

## License

MIT. See [LICENSE](LICENSE).
