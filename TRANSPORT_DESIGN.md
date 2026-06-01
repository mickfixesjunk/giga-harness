# TRANSPORT_DESIGN.md — pluggable transports for cross-host swarms

**Author:** giga agent
**Date:** 2026-06-01
**Status:** Draft for Mick review. On GO → 2-3 day implementation, ships as v0.3.0.
**Companion to:** [REMOTE_DESIGN.md](REMOTE_DESIGN.md) (the slice-and-merge architecture this builds on)

---

## 1. Problem

v0.2 shipped one transport: rsync over Tailscale SSH. It's great when you have a tailnet, painful when you don't:

- **Tailscale account required.** Free for personal use, but installing it on every box you want in the swarm + getting the auth click done is real friction.
- **Direct peer-to-peer connectivity.** Tailscale solves the NAT problem, but you're still relying on Tailscale's infra being up.
- **Doesn't fit some real setups.** Air-gapped corp networks, dev machines behind aggressive firewalls, sandbox environments.

Many users would prefer to push state to something they ALREADY have: a private GitHub repo, an S3 bucket, an Azure blob container. Some users want pure single-host (no remote at all). The transport should be **pluggable**, picked per-swarm, so the v1 slice-and-merge architecture isn't coupled to one specific network stack.

**Hard constraints:**

1. The slice-and-merge architecture (REMOTE_DESIGN.md §2) is unchanged. Plug-in only at the transport layer.
2. Existing v0.2 swarms keep working — the rsync+tailscale path becomes the default plug, not the only one.
3. Each plug is self-contained — adding a new one (e.g., GCS) doesn't require touching the others.

---

## 2. The trait

```rust
/// One transport. A swarm picks exactly one; all hosts in the swarm
/// must use the same transport (state has to round-trip through one
/// mechanism). For multi-trust-domain mesh setups, a future per-host
/// transport assignment could relax this — out of scope for v0.3.0.
pub trait Transport: Send + Sync {
    fn name(&self) -> &'static str;

    // ----- Slice-and-merge sync (mandatory) -----

    /// Long-running daemon's per-tick work. Push own slices to wherever
    /// peers can pick them up; pull peer slices into local inbox;
    /// keep the canonical TOML in sync.
    /// Idempotent. Errors logged + returned; daemon retries next tick.
    fn tick(&self, cfg: &Config, this_host: &str) -> Result<()>;

    /// One-shot peer bootstrap. Called by `giga add-host` and
    /// `giga add-agent --host` after the local TOML edit. Should
    /// (a) ensure the peer has the swarm dir + canonical TOML,
    /// (b) ensure peer has a per-host `this_host.toml`, and
    /// (c) leave the peer in a state where its own sync daemon
    ///     can pick up the swarm + start ticking.
    /// Best-effort: callers warn on failure rather than blocking
    /// local success (peer may be offline; sync recovers later).
    fn bootstrap_peer(&self, cfg: &Config, peer: &str, config_path: &Path) -> Result<()>;

    // ----- Command-on-peer (optional) -----

    /// Whether this transport supports synchronous command execution
    /// on a peer. `giga remote --host`, `giga sweep --host`,
    /// `giga launch --host` all require this.
    ///
    /// Returns false → those flags error with a clear "this
    /// transport doesn't support --host commands; run them
    /// locally on the peer instead" message.
    fn supports_remote_exec(&self) -> bool { false }

    /// Run a giga subcommand on a peer. Only called when
    /// supports_remote_exec is true. Default impl errors.
    fn run_remote(&self, _cfg: &Config, _peer: &str, _args: &[String]) -> Result<i32> {
        Err(anyhow!(
            "{}: --host commands not supported by this transport. \
             Run giga commands locally on the peer instead.",
            self.name(),
        ))
    }

    // ----- Bootstrap diagnostics (optional) -----

    /// Sanity-check the transport is correctly set up on this host.
    /// Called by `giga setup --remote-node --transport <kind>` at the
    /// end of the bootstrap. Default impl returns Ok (no diagnostics).
    fn self_check(&self) -> Result<()> { Ok(()) }
}

pub fn for_config(cfg: &Config) -> Result<Box<dyn Transport>> {
    match cfg.transport.kind.as_str() {
        "local"            => Ok(Box::new(LocalTransport)),
        "rsync+tailscale"  => Ok(Box::new(RsyncTailscaleTransport)),
        "git"              => Ok(Box::new(GitTransport::from_config(cfg)?)),
        // Future: "s3", "azure", "gcs", "webdav"
        other              => Err(anyhow!("unknown transport `{other}`")),
    }
}
```

### Key design call: `supports_remote_exec` as a per-plug capability

Command-on-peer connectivity is **orthogonal** to slice transport. Tailscale gives you both for free (SSH for commands, rsync for state). Git is push-only async — you can ship slices through it, but you can't make the peer run a command synchronously. S3 / cloud storage: same story.

Two ways to model this:
- **(a)** Pretend everything's the same: every transport must implement `run_remote` somehow (git transport could poll a `commands/<peer>/` dir in the repo). Hides the asymmetry behind 5-30s latency surprises.
- **(b) Make the asymmetry first-class.** Plugs declare capability; `--host` flags error cleanly when the active transport doesn't support remote-exec. Operator knows what they're getting up front.

This doc commits to (b). It's the more honest abstraction and makes future plugs (cloud-storage variants) trivial to add without inventing fake remote-exec channels.

---

## 3. TOML schema

```toml
[project]
name = "myswarm"

[paths]
wsl_inbox = "/tmp/myswarm-inbox"

# NEW: the swarm picks one transport. Optional — defaults to "local"
# (= no [[hosts]], single-host) when omitted. Existing v0.2 swarms
# with [[hosts]] but no [transport] section default to
# "rsync+tailscale" for backward compatibility.
[transport]
kind = "git"

# Per-kind config goes under the matching [transport.<kind>] table.
# Only the active kind's section is read; others are ignored.
[transport.git]
state_repo = "git@github.com:mick/myswarm-state.git"
# Optional: defaults to ~/.giga/swarm-state/<project>/
local_clone_dir = "/home/mick/.giga/swarm-state/myswarm"

[transport.rsync_tailscale]
# (no extra fields needed — uses [[hosts]].tailnet_hostname + ssh_user)

[transport.s3]
bucket = "mick-swarm-state"
region = "auto"               # "auto" for Cloudflare R2; explicit for AWS
endpoint = "https://...r2.cloudflarestorage.com"  # for R2/MinIO/B2
credentials_path = "~/.giga/transports/s3-creds.toml"  # contains access_key + secret_key
prefix = "swarm-state/"       # optional sub-prefix within the bucket

[[hosts]]
name = "wsl-b"
# Per-kind: tailnet_hostname + ssh_user only relevant for rsync+tailscale
# transport. Other transports just need `name` + the per-host overrides
# (remote_config_dir, remote_inbox_dir).
tailnet_hostname = "wsl-b.tail0000.ts.net"   # ignored under transport.kind != "rsync+tailscale"
ssh_user = "neo"
remote_config_dir = "/home/neo/.giga/configs/myswarm"
```

### Backward compatibility

- v0.2 swarms have no `[transport]` section. Loader treats them as:
  - `transport.kind = "rsync+tailscale"` if any `[[hosts]]` entry has `tailnet_hostname`
  - `transport.kind = "local"` if no `[[hosts]]` at all
- No TOML rewrite required. Tests in `tests/config.rs` cover the inference.

---

## 4. Three v0.3.0 plugs

### 4.1 `local` — single-host swarms (today's default behavior)

```rust
struct LocalTransport;

impl Transport for LocalTransport {
    fn name(&self) -> &'static str { "local" }
    fn tick(&self, _cfg, _this_host) -> Result<()> { Ok(()) }  // no-op
    fn bootstrap_peer(&self, _, _, _) -> Result<()> {
        Err(anyhow!("local transport can't bootstrap peers — swarm is single-host"))
    }
    fn supports_remote_exec(&self) -> bool { false }
}
```

Active when `[[hosts]]` is empty OR `transport.kind = "local"`. Everything that operates on peers becomes a no-op or clean error. The slice-and-merge fast-path (all-local channels writing direct to merged file) is in `post.rs` and unchanged.

### 4.2 `rsync+tailscale` — v0.2's default, lifted into the trait

Move the existing `sync.rs` body into `transports/rsync_tailscale.rs` as a struct implementing `Transport`. Pure refactor — zero behavior change. `supports_remote_exec` returns `true`; `run_remote` calls into the existing `remote.rs` SSH-passthrough code (which itself gets refactored into a function the trait method calls).

Tests: existing 200+ unit + 6 e2e + 5 local-chaos + 3 cross-host-chaos all keep passing post-refactor. CI verifies.

### 4.3 `git` — new

```rust
struct GitTransport {
    state_repo: String,
    local_clone_dir: PathBuf,
}

impl Transport for GitTransport {
    fn name(&self) -> &'static str { "git" }

    fn tick(&self, cfg: &Config, this_host: &str) -> Result<()> {
        self.ensure_clone()?;                            // first call: git clone if absent
        self.git_pull_rebase()?;                          // ~1-2s, gets peer slices + TOML
        self.mirror_repo_to_local_inbox(cfg, this_host)?; // peer slices → local <channel>.<host>.md
        self.mirror_local_to_repo(cfg, this_host)?;       // own slice growth → repo
        self.mirror_canonical_toml(cfg, this_host)?;      // if local TOML changed, copy → repo
        self.git_commit_and_push()?;                      // ~1-2s; idempotent if nothing changed
        Ok(())
    }

    fn bootstrap_peer(&self, _cfg, _peer, _config_path) -> Result<()> {
        // For git: peer bootstraps ITSELF when the operator runs
        // `giga setup --remote-node --transport git --repo <url>` on it.
        // The TOML edit on this side gets pushed automatically by the
        // next tick; peer's git pull picks it up.
        // So this is mostly a no-op, but we DO want to ensure the local
        // tick has committed the latest canonical TOML before returning
        // (so the new agent is in the repo before the operator looks
        // for them on the peer side).
        self.tick(_cfg, _cfg.this_host.as_deref().unwrap_or(""))
    }

    fn supports_remote_exec(&self) -> bool { false }

    fn self_check(&self) -> Result<()> {
        // Verify: git is on PATH, repo URL is set, local clone exists
        // or can be created, git push to the repo succeeds (auth works).
        Ok(())
    }
}
```

#### Repo layout (in the git state repo)

```
mick-swarm-state-myswarm/
├─ README.md                              (optional; "this is giga state, don't edit")
├─ giga-harness.toml                      (canonical TOML; everyone reads + this_host writes)
├─ agents/                                (templates rsync'd from operator's local agents/)
│   └─ <slug>.md
└─ slices/
    ├─ <channel>.<host-a>.md              (single-writer: only host-a touches)
    ├─ <channel>.<host-b>.md
    └─ ...
```

#### Conflict story

Each slice file in the repo is single-writer (only its owning host pushes it). Each host's git commit only touches its own slice files + maybe the canonical TOML (if this host edited it locally via add-agent/add-host). The canonical TOML IS multi-writer in principle, but:
- In practice it's written by the operator (single human) on one box
- If two hosts edit it concurrently: git push will fail on the second, daemon retries with `git pull --rebase` then `git push` — auto-resolved if changes are on different lines (different `[[agents]]` blocks usually). Real concurrent edits to the same TOML line are operator-on-operator races; falls back to "git rejected; please retry your add-agent".

For v0.3.0 we document the canonical-TOML-rebase-retry and ship; if it bites we add a single-writer model later (e.g., one host designated as the TOML writer).

#### Auth

User configures git auth via standard mechanisms (SSH keypair for `git@github.com:...` URLs, or PAT via `git config credential.helper` for HTTPS). No giga-specific auth surface. `setup --remote-node --transport git` runs `git ls-remote <repo>` as a smoke test + clear error on failure.

#### Latency

| Step | Time |
|---|---|
| git pull --rebase | 1-3s |
| mirror repo → inbox (local file copies) | <100ms |
| mirror inbox → repo | <100ms |
| git commit (no changes) | <100ms |
| git push (no changes) | 500ms-1s (still does a round trip) |
| git push (with changes) | 1-3s |
| **Total per tick** | ~3-7s |
| **End-to-end post-to-fire** | ~6-15s |

Slower than rsync+tailscale (~3-10s) but well within livable for swarm coordination.

---

## 5. Future plugs (not v0.3.0)

All fit the same `Transport` trait with no further interface changes:

| Plug | tick() | bootstrap_peer | remote_exec | When to ship |
|---|---|---|---|---|
| `s3` (incl. R2/B2/MinIO) | `aws s3 sync` slice files + canonical TOML | put TOML to bucket | false (or AWS SSM as a v0.4 niche) | v0.3.1 — most-requested next |
| `azure` (Azure Blob) | `az storage blob sync` | `az storage blob upload` | false | v0.3.2 if asked |
| `gcs` (Google Cloud Storage) | `gsutil rsync` | `gsutil cp` | false | v0.3.2 if asked |
| `webdav` (Nextcloud/self-hosted) | HTTP PUT/GET via reqwest | HTTP PUT | false | community-driven |
| `git-polled-cmds` | n/a (extension to `git`) | n/a | true (poll repo for command files) | only if remote-exec via git becomes a real ask |
| `syncthing-mount` | no transport — shared FS | no-op | false | "just use shared FS" pattern; mostly docs |

The s3 plug is the most-requested next step. ~150-200 LOC, plus a credentials file format. Defer to v0.3.1.

---

## 6. Refactor + implementation plan

### 6.1 Refactor pass (no new functionality)

1. **New `src/transport.rs`** — defines the trait + `for_config()` factory + the `Transport` config struct.
2. **New `src/transports/` dir** — `mod.rs`, `local.rs`, `rsync_tailscale.rs`, `git.rs`.
3. **Move existing `sync.rs` body** into `transports/rsync_tailscale.rs` as `struct RsyncTailscaleTransport`. `sync.rs` becomes a 20-line shim that loads config → picks transport → calls `tick()` in a loop.
4. **Move existing `remote.rs` SSH-passthrough** into a function that `RsyncTailscaleTransport::run_remote()` calls. Other transports get the default error stub.
5. **Update `Config`** — parse `[transport]` table; backward-compat fallback (`rsync+tailscale` when `[[hosts]]` non-empty, `local` otherwise).
6. **Tests** — every existing test (220+ unit, 6 e2e, 5 local-chaos, 3 cross-host-chaos) still passes. CI matrix unchanged.

Estimated effort: ~1 day.

### 6.2 Git plug

7. **`transports/git.rs`** — implements the trait via `git` subprocess calls.
8. **`giga setup --remote-node --transport git --repo <url>`** — installs git (if missing), git clones the state repo, runs `self_check()`, writes a hint about auth setup.
9. **`giga add-host --transport git --repo <url>`** — for swarms that don't yet have a transport but the operator wants to switch.
10. **Chaos tests** — extend `tests/cross_host_chaos.rs` to parameterize over transport: same R1-R3 invariants, this time with a real local-git repo simulating the shared state repo. Catches git-specific bugs (rebase conflicts, push races).

Estimated effort: ~1-1.5 days.

### 6.3 Docs

11. **REMOTE_DESIGN.md update** — §4 becomes "transports" instead of "rsync over Tailscale SSH"; cross-link to this doc.
12. **README.md update** — add a "Choosing a transport" decision table.
13. **REMOTE_QUICKSTART.md update** — add a git-transport variant of the 2-shot bootstrap.

Estimated effort: ~0.5 day.

**Total: 2.5-3 focused days. 5-7 commits. v0.3.0 release at the end.**

---

## 7. Open questions for Mick

1. **TOML schema flat vs. nested.** §3 commits to nested (`[transport.git]` etc.). Flat is simpler for the v1 case but doesn't scale to 7 plugs with disjoint fields. Confirm nested OK?
2. **Default-when-omitted backward-compat rule.** §3 says: if no `[transport]` section, infer from `[[hosts]]` presence (`local` vs `rsync+tailscale`). Alternative: require `[transport]` explicit in v0.3.0 and emit a migration warning for un-tagged v0.2 configs. Inference is friendlier; explicit is more honest. Recommend inference.
3. **`bootstrap_peer` for `git` transport.** §4.3 has the git plug's bootstrap as a no-op-plus-tick, because the peer bootstraps itself via `giga setup --remote-node --transport git`. Alternative: have add-host/add-agent push a "please bootstrap" marker into the repo + the peer picks it up. Adds 50 LOC for ~zero UX win — recommend the simpler "operator runs setup on each peer" model.
4. **Canonical TOML conflicts under git transport.** §4.3 documents the rebase-retry model. Worth more thought now or punt to v0.3.1 if it bites?
5. **`giga setup --remote-node` should learn `--transport <kind>`.** Today's hardcoded tailscale install becomes plug-aware. Reasonable, or keep `setup --remote-node` tailscale-only and add `setup --remote-node-git` as a sibling subcommand? (Lean: `--transport <kind>` is cleaner.)

---

## 8. Sign-off

On GO from Mick → 2-3 day implementation, single PR, ships as v0.3.0. Git plug + refactor of existing rsync+tailscale into the new shape, no behavior changes for existing swarms.
