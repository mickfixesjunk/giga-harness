# REMOTE_QUICKSTART.md — adding a second host to a giga-harness swarm

This is the operator runbook for the remote-channels feature
(per `REMOTE_DESIGN.md`). It takes a 2-agent swarm from "both
agents on one WSL box" to "one agent on each of two WSL boxes
talking transparently over a tailnet."

**Two roles in what follows:**
- **Operator host (host A)** — the box you sit at + run `giga` commands from. Already has a working swarm.
- **Remote node (host B)** — a bare WSL host you want to add as a swarm member. Has WSL installed; everything else gets installed during bootstrap.

**Time:** ~5-10 minutes (most of it is the interactive Tailscale auth on host B).

---

## On host B (the bare WSL box you're adding)

### 1. Install giga from the feature branch

```sh
git clone https://github.com/mickfixesjunk/giga-harness.git ~/giga-harness
cd ~/giga-harness
git checkout feat/remote-channels
cargo install --path .   # puts `giga` in ~/.cargo/bin
```

Confirm:

```sh
giga --version            # should be 0.1.12 (with remote-channels patches)
```

> _Once `feat/remote-channels` merges + a release ships, this collapses to_
> `curl -fsSL https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh | bash`.

### 2. Bootstrap as a remote node

```sh
giga setup --remote-node
```

This walks 6 idempotent steps:

| # | Step | What it does |
|---|---|---|
| 1 | WSL detection | refuses to run on non-WSL/Linux |
| 2 | rsync | apt-installs if missing |
| 3 | Tailscale | runs the official `install.sh` if missing |
| 4 | `tailscale up` | INTERACTIVE — prints an auth URL; visit it in a browser to authorize this node into your tailnet |
| 5 | Tailscale SSH | `tailscale set --ssh` so the operator can `giga remote --host <this>` without keypair exchange |
| 6 | Inbox dir | creates `~/projects/inbox` (override with `--inbox-dir <path>`) |

Use `--dry-run` to preview without changes.

When it finishes, **note the tailnet hostname it prints** (something like `wsl-box-b.tail1234.ts.net`). You'll paste that into the operator's config below.

---

## On host A (your operator host)

### 3. Install / update giga to the same feature branch

```sh
cd /path/to/giga-harness   # wherever your checkout lives
git fetch && git checkout feat/remote-channels && git pull
cargo install --path .
```

### 4. Convert your swarm to be remote-aware

Edit your swarm's canonical TOML (e.g. `~/.giga/configs/remote-test/giga-harness.toml`). Add the two new sections — everything else stays the same:

```toml
[[hosts]]
name = "wsl-a"                                    # your operator-host slug
tailnet_hostname = "wsl-a.tail1234.ts.net"        # paste what `tailscale status` shows here

[[hosts]]
name = "wsl-b"                                    # remote-node slug
tailnet_hostname = "wsl-box-b.tail1234.ts.net"    # paste what `giga setup --remote-node` printed on B

# ...then add `host = "wsl-a"` to existing-local-agent rows, e.g.:
[[agents]]
name = "test-a"
# ...existing fields...
host = "wsl-a"
```

Then create `~/.giga/configs/<swarm>/this_host.toml` (one line):

```toml
this_host = "wsl-a"
```

Verify with `giga validate`:

```sh
giga validate ~/.giga/configs/<swarm>/giga-harness.toml
```

### 4b. Register the new host in the swarm

```sh
giga add-host --name wsl-b \
              --tailnet-hostname wsl-b.tail1234.ts.net \
              --ssh-user <user-on-wsl-b> \
              --remote-config-dir /home/<user-on-wsl-b>/.giga/configs/<swarm>
```

Appends a `[[hosts]]` entry to the canonical TOML AND auto-bootstraps the new peer (mkdir + rsync the swarm dir + ensure peer's `this_host.toml`). `--no-bootstrap` skips the push if the peer isn't reachable yet.

**v0.3.8 — first-host migration is atomic.** When you add the FIRST host to a previously local-only swarm, `giga add-host` also:

- Registers the LOCAL host in `[[hosts]]` (using `$HOSTNAME` / `/etc/hostname`; override with `--this-host-name <NAME>`).
- Sets `host = "<local-host>"` on every existing host-less agent (they all implicitly lived on the local host).
- Writes `this_host.toml` next to the canonical config.

If post-edit validation fails, the TOML rolls back to the original. Pre-v0.3.8 the operator had to manually edit the TOML to break the chicken-and-egg between `[[hosts]]` and `this_host.toml`.

**Heads-up:** the placeholder `tailnet_hostname` written for the local host equals the local hostname (works under MagicDNS). If your tailnet hostname differs, edit the local `[[hosts]]` block manually — peers need it to push slices back.

**v0.3.9 — `this_host.local.toml` convention.** The per-host identity file is now named `this_host.local.toml` (was `this_host.toml`). Convention: any file in the swarm dir matching `*.local.toml` is host-private and never rsync'd between machines. `giga sync` / `giga add-host` bootstrap automatically excludes the pattern. If you do `rsync -av` of the swarm dir manually, the `*.local.toml` files are excluded by giga's own tooling but your bare rsync will overwrite them — use `rsync --exclude '*.local.toml'` for safety. Backward compat: the legacy name is still accepted at load time; new writes use the `.local.toml` name.

**Strict validation in multi-host swarms.** v0.3.8 also requires every `[[agents]]` block in a multi-host swarm (`[[hosts]]` non-empty) to declare `host = "<name>"` explicitly. The pre-v0.3.8 fallback (default to `this_host`) silently misrouted channels because the same canonical TOML resolved agents differently on each host. Existing swarms with host-less agents will fail `giga validate` after upgrade — fix by adding `host =` to each agent (or re-run `giga add-host --this-host-name <local>` on a still-local-only swarm to bulk-assign).

### 5. Add an agent on host B (single command — does everything)

```sh
giga add-agent --host wsl-b \
               --name test-b \
               --peer test-a \
               --role "test agent on box B" \
               --workdir /home/<user-on-wsl-b>/.giga/configs/<swarm>/workdirs/test-b
```

This single command:
1. Appends the new `[[agents]]` row + bilateral channel `test-a-test-b.md` to the canonical TOML
2. **Auto-bootstraps wsl-b**: rsyncs the whole swarm dir (TOML + `agents/<slug>.md` templates) to wsl-b, ensures `this_host.toml` exists
3. **Auto-scaffolds the new agent**: runs `giga init` remotely on wsl-b (host-aware: only touches wsl-b's agents, leaves wsl-a's workdirs alone), creating `test-b`'s workdir + `CLAUDE.md`

When the network/SSH is down, each of 2/3 individually warns + tells you the manual recovery command (`giga sync --once` or `giga remote --host wsl-b init`); the local TOML edit always succeeds.

### 6. Launch

Full launch on operator host A — this spawns the agent tabs PLUS two extra panes for `giga sync` and `giga merger`:

```sh
giga launch ~/.giga/configs/<swarm>/giga-harness.toml
```

On host B, bring up just the new agent's terminal via the operator:

```sh
giga launch --host wsl-b --only test-b
```

(This is equivalent to `giga remote --host wsl-b launch --only test-b`. A full `giga launch` on a cross-host swarm also spawns `giga sync` and `giga merger` panes per host — see `src/launch.rs` step 7.)

**Alternative — host the daemons inside an agent's Claude session (v0.3.6):** add `swarm_boss = true` to one agent per host in the TOML. That agent's `CLAUDE.md` will auto-include `Monitor(command: "giga sync --quiet")` and `Monitor(command: "giga merger --quiet")` lines, which it arms at session start. `giga launch` then skips the tmux daemon panes for that host. Fewer panes, LLM-in-the-loop for daemon errors. Trade-off: daemons die with the agent's session — pick a long-lived agent (e.g. `design`). See [SWARM_BOSS_DESIGN.md](SWARM_BOSS_DESIGN.md).

### 7. Smoke-test the round-trip

From host A's `test-a` session:

```sh
giga post test-a-test-b --as test-a --subject ping --body "hello from A"
```

Within ~10 seconds, `test-b` on host B should see the notification fire. Reply from B:

```sh
giga post test-a-test-b --as test-b --subject pong --body "hello back"
```

Within ~10 seconds, `test-a` on host A sees it.

---

## What if it doesn't work?

| Symptom | Likely cause | Fix |
|---|---|---|
| `tailscale status` fails on host B | `tailscale up` didn't complete | re-run step 2 |
| `ssh wsl-box-b.tail....ts.net` prompts for password | Tailscale SSH not enabled | re-run `sudo tailscale set --ssh` on B |
| `giga sync` complains "rsync not found" | step 2 didn't install rsync | `sudo apt install rsync` on the host complaining |
| Post on A doesn't appear on B | `giga sync` not running on A OR `giga merger` not running on B | check the sync + merger panes (or the swarm_boss agent's Monitors); restart if dead |
| Post on A appears as a slice file on B but not in the merged file | merger isn't running on B | start it: `giga merger --config <swarm>/giga-harness.toml` |
| Local-to-local post on a cross-host channel doesn't show up locally (v0.3.4 or older) | merger was load-bearing for local visibility pre-v0.3.5 | upgrade to v0.3.5+ — dual-write makes local visibility independent of merger liveness |
| swarm_boss agent crashed → no peer messages flowing | daemons died with the agent's session | restart the agent's Claude session; Monitors re-arm and daemons resume. Or fall back to tmux daemons: remove `swarm_boss = true` and re-run `giga launch` |
| `giga validate` errors `this_host = ... isn't in [[hosts]]` | typo between `this_host.toml` and `[[hosts]].name` | fix one to match the other |
| `giga remote --host X subcmd --flag value` is misparsed as putting `--flag value` into remote args | clap's trailing-args eats flagged args | put `--config` BEFORE the trailing list, or use `--`: `giga remote --host X --config ... -- subcmd --flag value` |

---

## Reference

- `REMOTE_DESIGN.md` — full architecture + tradeoffs
- `giga --help`, `giga setup --help`, `giga remote --help` — per-subcommand docs
