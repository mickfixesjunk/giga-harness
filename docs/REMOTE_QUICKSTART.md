# REMOTE_QUICKSTART.md — running a giga swarm across multiple hosts

This is the operator runbook for the **remote-channels** feature: spreading one
`giga` swarm across two (or more) machines on a Tailscale tailnet so agents on
different hosts coordinate over shared channels exactly as if they were
co-located. It takes a 2-agent swarm from "both agents on one WSL box" to "one
agent on each of two WSL boxes talking transparently."

Binary: `giga` (v0.6.54). All commands below are real, copy-pasteable, and use
flags that exist in the shipped CLI.

**Two roles in what follows:**
- **Operator host (host A)** — the box you sit at and run `giga` commands from.
  Already has a working local-only swarm.
- **Remote node (host B)** — a bare WSL host you want to add as a swarm member.
  Has WSL installed; everything else gets installed during bootstrap.

**Time:** ~5-10 minutes (most of it is the interactive Tailscale auth on host B).

New to giga? Start with [QUICKSTART.md](QUICKSTART.md) for the single-host happy
path, then come back here. See [COMMAND_REFERENCE.md](COMMAND_REFERENCE.md) for
every subcommand and flag.

---

## How it works: slices + merge

Before the steps, understand the model — because the troubleshooting table assumes it.

A cross-host channel `design-code.md` is backed by per-host **single-writer**
slice files in the same inbox dir: `design-code.<host>.md`, one per host.

1. **post** — `giga post` on host A appends the message to **its own slice**
   (`design-code.wsl-a.md`) **and** dual-writes the same frame into A's local
   merged `design-code.md`. The dual-write means a local post is visible locally
   immediately, independent of any daemon.
2. **sync** — `giga sync` on A pushes A's own slices (and the canonical TOML) to
   every peer over Tailscale SSH (rsync) — **push-only**.
3. **merge** — `giga merger` on host B sees the new bytes in `design-code.wsl-a.md`
   and appends them into B's merged `design-code.md`. The merger **excludes
   this_host** (B's own frames already got there via post's dual-write —
   re-merging would double-append).
4. **watch** — `giga watch` on B tails the merged `design-code.md` and surfaces
   the message into B's session, exactly as in a single-host swarm.

Reception is **push-only and symmetric**: each host pushes its own slices; each
host merges everyone else's. Merge progress is tracked by per-slice cursors at
`~/.giga/merge-cursors/<channel>/<slice_host>.pos`, persisted only after a
successful append — so delivery is **at-least-once** (never lost; rare duplicates
in a crash window). Don't hand-edit slice files: a shrunk slice resets the cursor.

---

## Choose a transport

A swarm picks **one** transport for its lifetime; **all hosts must use the same
kind**. It's set (or inferred) by `[transport].kind` in the TOML.

| Transport | When to use | Remote exec (`--host`, `remote`, `teleport`) | Auth |
|---|---|---|---|
| **`rsync+tailscale`** (default) | Hosts on a Tailscale tailnet, reachable peer-to-peer. The full-featured path. | **Yes** | Keyless via Tailscale SSH (tailnet identity) |
| **`git`** | Peers can't reach each other directly but all can reach a shared git remote (state repo). | **No** — run `giga` directly on each peer | Standard git (SSH keys for `git@`, credential helper for HTTPS) |

If `[transport]` is absent, the kind is **inferred**: `rsync+tailscale` when
`[[hosts]]` is non-empty, `local` otherwise.

**The important consequence:** `git` transport does **not** support remote
execution. `giga remote --host`, `giga launch --host`, `giga sweep --host`, and
`giga teleport` only work under `rsync+tailscale`. On a git swarm you SSH into the
peer yourself and run `giga` there.

This guide's main path is **rsync+tailscale**. The git 2-shot bootstrap is in
[Appendix: git transport](#appendix-git-transport).

---

## On host B (the bare WSL box you're adding)

### 1. Install giga

```sh
curl -sSfL https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh | bash
```

This puts `giga` on PATH (`~/.cargo/bin/giga`). Confirm it runs:

```sh
giga --version    # 0.6.54 (or newer)
```

Keep peers current later with `giga upgrade` from the operator host (it installs
on every peer and re-arms the watchers). To build from source as a contributor:
`git clone … && cargo install --path .`.

### 2. Bootstrap host B as a remote node

```sh
giga setup --remote-node
```

This walks 6 idempotent steps (`rsync+tailscale` path):

| # | Step | What it does |
|---|---|---|
| 1 | WSL detection | refuses to run on non-WSL/Linux (`v1` is WSL/Linux only) |
| 2 | rsync | apt-installs if missing |
| 3 | Tailscale | runs the official `install.sh` if missing |
| 4 | `tailscale up` | INTERACTIVE — prints an auth URL; visit it in a browser to authorize this node into your tailnet |
| 5 | Tailscale SSH | `tailscale set --ssh` so the operator can `giga remote --host <this>` without keypair exchange |
| 6 | Inbox dir | creates `~/projects/inbox` (override with `--inbox-dir <path>`) |

Use `--dry-run` to preview without changes.

When it finishes, **note the tailnet hostname it prints** (something like
`wsl-box-b.tail1234.ts.net`). You'll pass that to `giga add-host` on the operator.

> Heads-up: `setup --remote-node` also prints "next commands" to run on the
> operator. Under `rsync+tailscale` those are correct. (The `--transport git`
> path prints a buggy `giga add-host --transport git --repo …` invocation — see
> the git appendix; `add-host` has no such flags.)

---

## On host A (your operator host)

### 3. Make sure giga is current

```sh
giga --version   # match host B; `giga upgrade` if older
```

### 4. Register host B (this also migrates your swarm to multi-host)

This is the **primary** path. `giga add-host` registers the peer **and**, when
your swarm was previously local-only, performs an atomic **first-host migration**
in one shot.

```sh
giga add-host --name wsl-b \
              --tailnet-hostname wsl-box-b.tail1234.ts.net \
              --ssh-user <user-on-wsl-b> \
              --remote-config-dir /home/<user-on-wsl-b>/.giga/configs/<swarm>
```

Flags (all optional except `--name` / `--tailnet-hostname`):
- `--name` — host slug (matches `[[hosts]].name` and `agent.host`).
- `--tailnet-hostname` — the FQDN host B printed in step 2.
- `--ssh-user` — OS user on the peer (defaults to `$USER`).
- `--remote-config-dir` — where the config lives on the peer (defaults to the
  local config dir — homogeneous-path setup).
- `--remote-inbox-dir` — where the inbox lives on the peer (defaults to the local
  inbox path).
- `--this-host-name <NAME>` — name to register THIS host as during the first-host
  migration (auto-detected from `$HOSTNAME` / `/etc/hostname`; ignored once
  `[[hosts]]` already exists).
- `--no-bootstrap` — skip the SSH/rsync push (use when the peer isn't reachable
  yet).
- `--dry-run` — print the planned change; write nothing.

**What it does under the hood:**
- Appends a `[[hosts]]` entry for `wsl-b`.
- **First-host migration** (only when the swarm had no `[[hosts]]` yet): also
  registers the LOCAL host as a second `[[hosts]]` entry, sets
  `host = "<local-name>"` on **every** host-less agent (they implicitly lived
  here), and writes `this_host.local.toml` next to the config. Reloads and
  re-validates; **rolls back** the whole edit (and removes the just-written
  identity file) on validation failure.
- Unless `--no-bootstrap`: rsyncs the config dir to the peer (excluding
  `*.local.toml` and `workdirs/`) and ensures the peer has its own identity file
  — best-effort.

> **Verify the local host's `tailnet_hostname`.** The migration writes the local
> host's `tailnet_hostname` as a **placeholder equal to the local name**. That
> works under MagicDNS, but if your real FQDN differs, peers can't push slices
> back until you fix it. Edit the local `[[hosts]]` block (or re-run with the
> right value) and confirm with `giga hosts`.

**Manual fallback (rare):** if you'd rather hand-edit, add `[[hosts]]` entries
for both hosts, put `host = "<host>"` on every agent, and create
`this_host.local.toml` (one line: `this_host = "wsl-a"`). This is exactly what the
migration automates — prefer `add-host`.

Confirm topology and identity:

```sh
giga hosts                 # shows each host, its agents, and whether this_host matches
giga validate              # parse + schema check, no filesystem writes
```

> **Why `host =` is required in a multi-host swarm.** Once `[[hosts]]` is
> non-empty, every `[[agents]]` block must declare `host = "<name>"` — the same
> canonical TOML is read on every machine, so an implicit "local" would resolve
> differently per host and misroute channels. `giga validate` enforces this.
> The per-host identity is read from `this_host.local.toml` (the legacy
> `this_host.toml` is still accepted, but new tooling writes `.local.toml`; any
> `*.local.toml` is host-private and excluded from sync — use
> `rsync --exclude '*.local.toml'` if you ever rsync the dir by hand).

### 5. Add an agent on host B (one command — does everything)

```sh
giga add-agent --host wsl-b \
               --name test-b \
               --peer test-a \
               --role "test agent on box B" \
               --workdir /home/<user-on-wsl-b>/.giga/configs/<swarm>/workdirs/test-b
```

Flags of note: `--host` (must match a `[[hosts]].name`; non-local triggers the
peer auto-bootstrap), `--name`, `--peer` (repeatable; one bilateral channel per
peer), `--role`, `--workdir` (absolute, no leading `~`).

This single command:
1. Appends the new `[[agents]]` row + a bilateral channel `test-a-test-b.md`
   (sorted-alphabetical filename) to the canonical TOML.
2. **Auto-bootstraps wsl-b**: rsyncs the whole config dir (TOML +
   `agents/<slug>.md` templates) to wsl-b and ensures its identity file exists.
3. **Auto-scaffolds the new agent**: runs host-aware `giga init` on wsl-b
   remotely (touches only wsl-b's agents; leaves wsl-a's workdirs alone), creating
   `test-b`'s workdir + `AGENTS.md`.

When the network/SSH is down, steps 2 and 3 each warn individually and print the
manual recovery command (`giga sync --once` or `giga remote --host wsl-b init`);
the local TOML edit always succeeds.

### 6. Launch — and where the daemons run

A cross-host swarm runs **three** daemons per host: `giga watch` (tails channels,
per agent), `giga sync` (pushes own slices + TOML to peers), and `giga merger`
(appends peer slices into local merged files). `sync` and `merger` no-op on a
local-only swarm.

There are two ways to run `sync` + `merger` on a host:

**(a) Dedicated daemon panes.** A **full** `giga launch` spawns `giga-sync` and
`giga-merger` panes alongside the agent tabs:

```sh
giga launch ~/.giga/configs/<swarm>/giga-harness.toml
```

**(b) A swarm_boss agent.** Designate one agent per host as the **swarm_boss**; it
arms `giga sync --quiet` and `giga merger --quiet` as Monitors inside its own
session (no separate daemon panes). Promote/demote with the subcommand (it re-runs
`giga init` to regenerate the boss's `AGENTS.md` supervision section):

```sh
giga set-swarm-boss test-a              # promote
giga set-swarm-boss test-a --unset      # demote
```

Rules: **at most one swarm_boss per host**, and it **must be `platform = "wsl"`**
(sync + merger are POSIX-only). The boss section only appears in `AGENTS.md` when
`[[hosts]]` is non-empty. Trade-off: the daemons live in that agent's session — if
it dies they stop, so pick a long-lived agent. When a host has a swarm_boss,
`giga launch` skips the daemon panes on that host (the boss covers it).

**The daemon-spawn rule (matters for `--only`):** `giga launch --only <agent>`
does **NOT** spawn daemon panes — it joins the existing session incrementally.
So a peer brought up only with `--only` needs **either** a swarm_boss **or** a
separately-started `giga sync` + `giga merger` for cross-host messages to flow.

Bring up just the new agent's terminal on host B via the operator:

```sh
giga launch --host wsl-b --only test-b
```

This is sugar for `giga remote --host wsl-b launch --only test-b`. Because it's an
`--only` launch, it does **not** start daemons on wsl-b — so make `test-b` (or
another wsl-b agent) the swarm_boss, or start `giga sync` + `giga merger` on wsl-b
yourself.

### 7. Smoke-test the round-trip

From host A's `test-a` session:

```sh
giga post test-a-test-b --as test-a --subject ping --body "hello from A"
```

Within ~10 seconds, `test-b` on host B should see the notification fire. Reply
from B:

```sh
giga post test-a-test-b --as test-b --subject pong --body "hello back"
```

Within ~10 seconds, `test-a` on host A sees it. (`giga post` takes a positional
CHANNEL — `.md` optional — plus `--as`, `--subject`, and `--body`; omit `--body`
to read from stdin.)

---

## Running giga on a peer: `giga remote`

`giga remote` is the SSH-passthrough primitive that **all** `--host` sugar is
built on (`launch --host`, `sweep --host`, `add-agent --host`, `upgrade`'s peer
install, `teleport`'s remote init/launch):

```sh
giga remote --host wsl-b sweep
```

It runs `ssh <user>@<tailnet_hostname> bash -lc 'cd <remote_dir> && giga <args>'`,
inherits your stdio, and propagates the remote exit code. It needs **Tailscale SSH**
(keyless tailnet-identity auth, enabled by `setup --remote-node`) and works
**only under `rsync+tailscale`** — `local`/`git` error cleanly and tell you to run
giga directly on the peer.

**Trailing-arg footgun:** everything after the subcommand is captured verbatim and
sent to the remote subcommand. Put `--config` (the only flag `giga remote` itself
takes besides `--host`) **before** the trailing list, or separate with `--`:

```sh
giga remote --host wsl-b --config <swarm>/giga-harness.toml -- sweep --owed-by test-b
```

---

## Moving an agent between hosts: `giga teleport`

To relocate a running agent from one host to another (rsync+tailscale only):

```sh
giga teleport --to wsl-b test-a
```

Flags: positional `<AGENT>`; `--to <HOST>` (required destination, must be in
`[[hosts]]`); `--from <HOST>` (source — defaults to the agent's current `host`);
`--keep-running` (don't kill the source pane(s); prints manual teardown instead);
`--dry-run` (print every step, no side effects).

What it does: rsyncs the agent's workdir source→target, prepends a teleport banner
to the target `HANDOVER.md`, sets `agent.host = <target>` in the TOML, runs
`giga sync --once`, then remote `giga init` + `giga launch --only <agent>` on the
target, and finally kills the source pane(s) (unless `--keep-running`).

**Caveats:**
- **Homogeneous-path assumption** — `agent.workdir` is used **verbatim** on both
  hosts (heterogeneous paths unsupported in v1).
- **Does NOT move:** channel slices (the agent's past posts stay in the source's
  slice forever — still visible swarm-wide via merge), `~/.claude/` conversation
  history (per-machine), and cursors (reset → the agent's first watch tick on the
  target **replays the channel history from byte 0**).
- The sync/init/launch steps are best-effort; on failure teleport prints the
  manual recovery commands.

---

## What if it doesn't work?

| Symptom | Likely cause | Fix |
|---|---|---|
| `tailscale status` fails on host B | `tailscale up` didn't complete | re-run step 2 |
| `ssh wsl-box-b.tail….ts.net` prompts for a password | Tailscale SSH not enabled | re-run `sudo tailscale set --ssh` on B |
| `giga sync` complains "rsync not found" | step 2 didn't install rsync | `sudo apt install rsync` on the host complaining |
| `giga sync` exits "no [[hosts]] declared … Exiting." | the swarm is still local-only | run `giga add-host` (step 4) first |
| Post on A doesn't appear on B | `giga sync` not running on A OR `giga merger` not running on B | check the sync + merger panes (or the swarm_boss agent's Monitors); restart if dead. Remember `--only` launches don't spawn daemons |
| Post on A appears as a slice file on B but not in B's merged channel | merger isn't running on B | start it: `giga merger --config <swarm>/giga-harness.toml` |
| swarm_boss agent crashed → no peer messages flowing | daemons died with the agent's session | restart the agent's session (Monitors re-arm) or fall back to panes: `giga set-swarm-boss <slug> --unset` then a full `giga launch` |
| `giga validate` errors `this_host = … isn't in [[hosts]]` | typo between `this_host.local.toml` and `[[hosts]].name` | fix one to match the other |
| `giga validate` errors that an agent has no `host` | multi-host swarm requires `host =` on every agent | add `host = "<name>"` to each agent (or re-run `giga add-host` on a still-local-only swarm to bulk-assign) |
| Peer never receives your slices though sync looks fine | local host's `tailnet_hostname` is the placeholder (= local name) and your real FQDN differs | fix the local `[[hosts]].tailnet_hostname`; confirm with `giga hosts` |
| `giga remote --host X subcmd --flag value` misparses `--flag value` into remote args | clap captures trailing args verbatim | put `--config` BEFORE the trailing list, or use `--`: `giga remote --host X --config … -- subcmd --flag value` |
| `giga remote --host X …` errors that the transport can't exec remotely | you're on `git`/`local` transport | git/local have no remote exec — SSH into the peer and run `giga` directly |

---

## Appendix: git transport

Use git when peers can't reach each other directly but all can reach a shared git
remote (the **state repo**). Reminder: **no remote exec** — `giga remote`,
`--host` sugar, and `teleport` don't work; run `giga` directly on each peer.

### Bootstrap (2-shot, each peer)

On each peer (including the operator host):

```sh
giga setup --remote-node --transport git --repo git@github.com:you/swarm-state.git
```

The git path runs 5 steps: WSL check; install git; `git ls-remote <repo>` auth
smoke-test (it does **NOT** clone); install rsync; create the inbox dir.

> **Ignore the `add-host` command it prints.** `setup --remote-node --transport git`
> prints `giga add-host --transport git --repo … --name …`, but `add-host` has
> **no** `--transport` or `--repo` flags — that invocation will fail with an
> unknown-flag error. Configure git transport in the TOML instead (next).

### Configure the swarm for git

Set the transport in the canonical `giga-harness.toml` directly:

```toml
[transport]
kind = "git"

[transport.git]
state_repo = "git@github.com:you/swarm-state.git"
# local_clone_dir = "~/.giga/swarm-state/<project>/"   # optional; this is the default
```

Then register hosts normally (omit the bogus flags):

```sh
giga add-host --name wsl-b --tailnet-hostname wsl-b.example   # tailnet_hostname unused for exec under git, but the field is required
```

Run `giga sync` + `giga merger` on **each** peer (they pull/rebase the state repo,
mirror slices↔inbox, and commit/push). The first `giga sync` tick lazily clones
the repo. The canonical TOML is whole-file mirrored (last-writer-wins — avoid
concurrent multi-operator edits); slices are append-only and conflict-free.

---

## Reference

- [QUICKSTART.md](QUICKSTART.md) — single-host getting started.
- [COMMAND_REFERENCE.md](COMMAND_REFERENCE.md) — every subcommand + flag.
- [MANUAL_SETUP.md](MANUAL_SETUP.md) — hand-rolling a swarm without `giga setup`.
- [CLAUDE_OPERATOR.md](../templates/CLAUDE_OPERATOR.md) — operating a swarm from inside a Claude session (print it with `giga claude-operator`).
- [Back to the README](../README.md).
- Per-subcommand help: `giga setup --help`, `giga add-host --help`,
  `giga remote --help`, `giga teleport --help`, `giga sync --help`,
  `giga merger --help`.
