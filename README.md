# giga-harness

[![ci](https://github.com/mickfixesjunk/giga-harness/actions/workflows/ci.yml/badge.svg)](https://github.com/mickfixesjunk/giga-harness/actions/workflows/ci.yml)

A coordination harness for running N parallel AI coding agents (Claude Code, Codex, etc.) that talk to each other through file-based inboxes. One terminal tab per agent; each agent reads its own `CLAUDE.md`, watches a shared inbox, and posts back when it has something to say.

Agents can run on **one machine** (default — append-only inbox files, polling watcher) or across **multiple machines** via a pluggable transport (v0.3+):

- **`rsync+tailscale`** — rsync over Tailscale SSH. Lowest latency (~5s), needs Tailscale.
- **`git`** — a private git repo as the shared state store. Slightly slower (~10s), works through any firewall, no tailnet needed.
- **`local`** — single-host (default when no `[[hosts]]`).

See [§ Multi-host swarms](#multi-host-swarms-cross-host-channels) for the operator flow and [TRANSPORT_DESIGN.md](TRANSPORT_DESIGN.md) for the plug architecture.

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
giga --version    # should be 0.1.12 or newer
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
- **[REMOTE_QUICKSTART.md](REMOTE_QUICKSTART.md)** — operator runbook for adding a second host to a swarm via tailnet.
- **[REMOTE_DESIGN.md](REMOTE_DESIGN.md)** — the design + architecture for cross-host channels (slice-and-merge, Tailscale SSH transport).
- `giga --help`, `giga <subcommand> --help` — every subcommand has detailed help.

## Subcommands at a glance

### Single-host (the default)

| Command | What it does |
|---------|--------------|
| `giga setup` | One-command bootstrap. Launches Claude Code with a baked-in prompt that walks you through scaffolding a new swarm end-to-end. Run this from any project directory. |
| `giga validate [config]` | TOML schema + cross-reference check. Flags on-disk inbox files not enrolled in `[[channels]]`. No side effects. |
| `giga init [config]` | Creates inbox files + per-agent `CLAUDE.md` (idempotent). Registers the swarm in `~/.giga/swarms.toml`. Host-aware: in a multi-host swarm, only scaffolds agents whose `host` matches `this_host`. |
| `giga add-agent --name X --workdir Y --role "..." [--code-root Z] [--host H] --peer A [--peer B]` | Scaffold a new agent — `[[agents]]` + `[[channels]]` + broadcast participation + a stub template. `--code-root` lets the agent edit a shared codebase from an isolated workdir. `--host` puts the agent on a peer host (auto-bootstraps the peer + scaffolds the workdir there). `--dry-run` previews. |
| `giga add-channel --participants A,B [--file ...]` | Append a new bilateral channel to the canonical TOML. v1 supports 2 participants only; auto-derives `<a>-<b>.md` filename. |
| `giga launch [config] [--host H]` | One terminal per agent. `--terminal <mode>` picks the launcher: `auto`, `mac-terminal` (Terminal.app), `tmux`, `wt`, or `print`. `--only <a,b>` spawns just the named agents (non-disruptive add). `--new-window` forces a fresh wt window. `--host H` runs launch on a peer over SSH. Cross-host swarms also spawn `giga sync` + `giga merger` panes per host. |
| `giga sweep [config] [--host H]` | Tabulate every channel's last message + open `WAITING ON` tags. `--host H` runs sweep on the peer (output streams back). |
| `giga post <channel> --as <agent> --subject ...` | Append a properly-formatted message. `<channel>` accepts the bare name or `.md`-suffixed form. Cross-host channels auto-route to per-host slice files (`<channel>.<this_host>.md`). |
| `giga watch --as <agent>` | Long-running watcher — auto-tracks every channel where the agent participates. Run under Claude Code's `Monitor` tool. |
| `giga switch --runtime claude [<account>]` | Multi-account credential manager. See [§ Multi-account switching](#multi-account-switching). |

### Multi-host (cross-host channels)

| Command | What it does |
|---------|--------------|
| `giga setup --remote-node` | Bootstrap a bare WSL host as a swarm peer: installs Tailscale + rsync, runs `tailscale up` (interactive auth), enables Tailscale SSH, creates the inbox dir. Run on the new host first; then `add-host` from operator side. |
| `giga add-host --name H --tailnet-hostname FQDN [--ssh-user U] [--remote-config-dir P] [--remote-inbox-dir P]` | Append a `[[hosts]]` entry to the canonical TOML and (by default) auto-bootstrap the new peer: mkdir + rsync swarm dir + ensure peer's `this_host.toml`. `--no-bootstrap` opts out. |
| `giga remote --host H -- <subcommand>` | SSH passthrough primitive — runs any giga subcommand on the peer over Tailscale SSH, streaming stdout/stderr back. `--host H` flags on add-agent/sweep/launch are sugar over this. Note: put trailing args after `--`. |
| `giga sync [--once] [--dry-run]` | Long-running daemon — every 3s, rsync the canonical TOML + own slice files to each peer. `--once` runs a single tick. `--dry-run` previews. Auto-spawned by `giga launch` on cross-host swarms. |
| `giga merger [--once]` | Long-running daemon — polls all `<channel>.<host>.md` slice files and appends new bytes to the watched `<channel>.md`. Auto-spawned by `giga launch` on cross-host swarms. |

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

**How it works.** `~/.claude/.credentials.json` is the real, live credentials file claude reads + refreshes. `~/.claude-accounts/<name>.json` is a snapshot per account, plus `.active` marks which one is currently live. Switching copies the live file back to its snapshot (preserving any in-place OAuth refreshes), then copies the target snapshot into the live file. No symlinks — `/login` and silent token refreshes use write-temp-then-rename, which destroys symlinks, so we use plain file copies instead.

**Limitations.** Claude-only today (`--runtime claude`). Linux/macOS/WSL only — Windows-native isn't wired up yet. Whole-swarm switch, not per-agent.

## Multi-host swarms (cross-host channels)

A giga swarm can span multiple physical machines on a tailnet. Agents on different hosts participate in the same channels as if they were local; the existing single-host model stays the fast-path for all-local channels.

### How it works (one-paragraph)

Each cross-host channel has per-host slice files `<channel>.<host>.md` next to the merged `<channel>.md`. When an agent posts on a cross-host channel, `giga post` **dual-writes** the same frame to its host's slice (for sync to ship to peers) AND to the merged file (so the local watcher sees it immediately — independent of any daemon's liveness). A local `giga sync` daemon rsyncs each host's own slice files to peers over Tailscale SSH; a local `giga merger` daemon appends incoming PEER slice bytes to the merged file. Channels with all participants on `this_host` skip the slice path entirely (fast-path direct write to the merged file). Auth is tailnet identity — no SSH key exchange, no `authorized_keys` files. See [REMOTE_DESIGN.md](REMOTE_DESIGN.md) and [REMOTE_DUAL_WRITE_DESIGN.md](REMOTE_DUAL_WRITE_DESIGN.md) for the full architecture.

### Choose a transport

| Transport | Connectivity | Latency | Setup | When |
|---|---|---|---|---|
| `rsync+tailscale` | mutual tailnet membership | ~5s | install Tailscale on each peer, click auth URL | you already have a tailnet OR want minimal latency |
| `git` | any git host (GitHub/GitLab/self-hosted) | ~10s | create a private state repo + each peer clones it | no tailnet, behind firewalls, already have git auth set up |
| `local` | (single-host only) | n/a | nothing | swarm fits on one machine |

### Bootstrap a new tailnet peer — 2 shots (rsync+tailscale)

Install the giga binary on both hosts (Linux/macOS/WSL):

```sh
curl -sSfL https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh | bash
```

**Shot 1 — on the NEW host (interactive: Tailscale auth URL):**

```sh
giga setup --remote-node                                # defaults to --transport rsync+tailscale
```

Installs Tailscale + rsync, runs `tailscale up`, enables Tailscale SSH, creates `~/projects/inbox`. ~5 min.

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

Post-to-fire latency: ~3-10 seconds.

### Bootstrap a new git-transport peer — 2 shots

Create a private state repo once per swarm (any git host works):

```sh
gh repo create mick-swarm-state-<swarm> --private --confirm
```

Add the transport stanza to your swarm's `giga-harness.toml`:

```toml
[transport]
kind = "git"

[transport.git]
state_repo = "git@github.com:mick/mick-swarm-state-<swarm>.git"
```

Install the giga binary on both hosts (same one-liner as above). Then:

**Shot 1 — on the NEW host:**

```sh
giga setup --remote-node --transport git \
           --repo git@github.com:mick/mick-swarm-state-<swarm>.git
```

Installs git + rsync, smoke-tests repo auth (`git ls-remote`), creates `~/projects/inbox`.

**Shot 2 — on the OPERATOR host (edit TOML to register the peer):**

```toml
[[hosts]]
name = "wsl-b"
tailnet_hostname = "unused"     # required field but ignored under git transport
```

```sh
giga add-agent --host wsl-b --name <slug> --peer <existing> --role "..." \
               --workdir /home/neo/.giga/configs/<swarm>/workdirs/<slug>
# Then on wsl-b directly (giga remote --host doesn't work under git transport):
giga launch --only <slug>
```

The new agent's terminal needs to be brought up ON the peer (no remote-exec under git). Post-to-fire latency: ~10-15 seconds.

> **Heads-up:** with `transport.kind = "git"`, the `--host` flags on `sweep` / `launch` / `add-agent`'s auto-launch don't work — they need synchronous reach to the peer (SSH), which git doesn't provide. Operator orchestrates each peer's local commands; the swarm coordinates over git for slice traffic. See [TRANSPORT_DESIGN.md §2](TRANSPORT_DESIGN.md) (`supports_remote_exec`).

Full operator runbook with troubleshooting + the `[[hosts]]` schema deep dive: **[REMOTE_QUICKSTART.md](REMOTE_QUICKSTART.md)**.

### Schema additions (recap)

```toml
[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0000.ts.net"
ssh_user = "neo"                                       # optional; defaults to $USER
remote_config_dir = "/home/neo/.giga/configs/<swarm>"  # optional; defaults to local path
remote_inbox_dir  = "/tmp/<swarm>-inbox"               # optional; defaults to paths.wsl_inbox

[[hosts.paths]]                                        # v0.3.2: per-host inbox override
wsl_inbox = "/home/<their-user>/projects/inbox"

[[agents]]
host = "wsl-b"                                         # which host this agent runs on
swarm_boss = true                                      # v0.3.6: hosts sync + merger as Monitors (optional, one per host)
```

Plus a one-line `this_host.toml` next to the canonical config on each host:

```toml
this_host = "wsl-a"
```

### Where the sync + merger daemons run (v0.3.6: `swarm_boss`)

Each multi-host host needs one `giga sync` daemon (pushes its own slices to peers) and one `giga merger` daemon (pulls peer slices into local merged files). By default `giga launch` spawns them as tmux panes alongside the agent panes.

For a leaner topology, flag one agent per host as the **swarm boss**:

```toml
[[agents]]
name = "design"
swarm_boss = true
host = "wsl-a"
```

That agent's `CLAUDE.md` (generated by `giga init`) auto-includes two `Monitor` lines for `giga sync --quiet` and `giga merger --quiet`. The agent arms them at session start alongside its inbox watcher — three Monitors total instead of one. `giga launch` then skips the tmux daemon panes for that host (daemons live in the boss agent's session instead).

Trade-off: when the swarm_boss agent's Claude session ends, the daemons die — cross-host comms degrade for that host until restart. Pick a long-lived, low-churn agent as the boss (e.g. `design`, not an actively-iterating code agent). The Monitor architecture also keeps an LLM in the loop for daemon errors — rsync failures and peer-unreachable events surface as notifications the agent can flag rather than dying silently in a tmux pane. See [SWARM_BOSS_DESIGN.md](SWARM_BOSS_DESIGN.md) for the trade-offs and failure modes.

### When NOT to use it (current v1 limitations)

- Only WSL/Linux peers in v1; Windows-native peers need WSL.
- Push topology is O(N²) connections per tick — fine up to ~5 hosts; hub-and-spoke for more.
- Tailscale only (rsync over Tailscale SSH); S3/R2 cloud-storage transport is the v1.1 follow-up.

## Key concepts

- **`workdir`** — the agent's isolated launch context. Their `CLAUDE.md` lives here; `claude` opens here. Default: `~/.giga/configs/<project>/workdirs/<agent>/`.
- **`code_root`** — *optional, separate from workdir.* The directory the agent actually edits in. Lets multiple agents share a single codebase while each has their own clean workdir. Set per-agent in TOML or via `giga add-agent --code-root <path>`.
- **Registry (`~/.giga/swarms.toml`)** — auto-maintained map of `code_root → config_path`. Lets `giga <command>` work from anywhere under your codebase, no `cd` required.
- **Window titles + reply prefixes** — every agent's terminal window is titled with their slug; every reply they post starts with `[slug]`. Hard to lose track of who's talking.
- **`[[hosts]]` + `this_host`** *(multi-host only)* — `[[hosts]]` enumerates the physical machines in the swarm; each agent's `host` field names which one it runs on. Each host has a one-line `this_host.toml` next to its canonical config telling its local giga which host identity it is. Absent for all-local swarms — today's behavior, untouched.
- **Slice files** *(multi-host only)* — `<channel>.<host>.md` is the single-writer wire format. Each host appends only to its own slice; the local merger reads everyone's slices and appends to the merged `<channel>.md` that the watcher tails. Append-only invariant preserved by construction.

## License

MIT. See [LICENSE](LICENSE).
