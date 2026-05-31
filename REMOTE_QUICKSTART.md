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

### 5. Add an agent on host B

```sh
giga add-agent --host wsl-b \
               --name test-b \
               --peer test-a \
               --role "test agent on box B" \
               --workdir /home/<you>/.giga/configs/<swarm>/workdirs/test-b
```

This appends the new `[[agents]]` row to the canonical TOML, adds a bilateral channel `test-a-test-b.md`, and (because `--host` names a non-local peer) auto-bootstraps host B:

- `mkdir -p` the swarm dir on B (at the host's `remote_config_dir` if set, otherwise the local absolute path)
- rsync the canonical `giga-harness.toml` to B
- create B's `this_host.toml` if it doesn't already exist

You'll see `auto-bootstrap: pushing canonical TOML to wsl-b...` in the output. If the network/SSH is down at the time, the local TOML edit still succeeds and the auto-bootstrap warns instead of failing; re-run `giga sync --once` later to recover.

> _One thing it does NOT do yet: scaffold the per-agent CLAUDE.md + workdir on B. You'd run `giga remote --host wsl-b init` after to do that._

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
| Post on A doesn't appear on B | `giga sync` not running on A OR `giga merger` not running on B | check the sync + merger panes; restart if dead |
| Post on A appears as a slice file on B but not in the merged file | merger isn't running on B | start it: `giga merger --config <swarm>/giga-harness.toml` |
| `giga validate` errors `this_host = ... isn't in [[hosts]]` | typo between `this_host.toml` and `[[hosts]].name` | fix one to match the other |

---

## Reference

- `REMOTE_DESIGN.md` — full architecture + tradeoffs
- `giga --help`, `giga setup --help`, `giga remote --help` — per-subcommand docs
