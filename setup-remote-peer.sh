#!/usr/bin/env bash
# setup-remote-peer.sh - Prepare a WSL host to act as a remote peer in a
# giga-harness swarm using rsync over Tailscale SSH.
#
# Run this script inside the WSL distro on EVERY host that will participate
# in the remote-channel swarm, including the primary one you already use.
# It is idempotent - safe to re-run.
#
# Why Tailscale SSH (and not openssh-server + ssh-copy-id):
#   Tailscale SSH uses your tailnet identity for auth. Once enabled on each
#   host, any host can ssh/rsync to any other peer with NO keypair exchange,
#   NO authorized_keys file, NO per-pair setup. Adding the Nth host scales
#   as O(1) per host instead of O(N) per pair.
#
# What it does:
#   1. Sanity-check we are running inside WSL.
#   2. Check the inbox dir for existing channel files (heads-up so you don't
#      accidentally mix a test swarm into a production inbox).
#   3. Install rsync via apt.
#   4. Verify the `tailscale` command is present and the daemon is running.
#   5. Enable Tailscale SSH on this host (`tailscale set --ssh`).
#   6. Create the inbox directory.
#   7. Print the tailnet hostname + next manual steps.
#
# What it does NOT do (intentional):
#   - Install Tailscale itself. Run `curl -fsSL https://tailscale.com/install.sh | sh`
#     first, then `sudo tailscale up`, then this script.
#   - Configure tailnet ACLs. The default tailnet ACL allows all your devices
#     to talk to each other — that's enough. Only relevant if you've
#     customized your ACL to restrict SSH.
#   - Install the `giga` binary. Do that with the install.sh at the repo root,
#     or `cargo install giga-harness` once the remote-channels feature ships.
#   - Edit giga-harness.toml. The schema for [[hosts]] is documented in
#     REMOTE_DESIGN.md; until the feature ships, leave the TOML alone.
#
# Usage:
#   bash setup-remote-peer.sh                          # defaults
#   bash setup-remote-peer.sh --inbox-dir /opt/inbox   # custom inbox path
#   bash setup-remote-peer.sh --no-tailscale-ssh       # skip ts-ssh enable
#                                                       # (if you manage it
#                                                       #  yourself)
#   bash setup-remote-peer.sh --dry-run                # print actions, no changes
#
# Requires:
#   - WSL2 (or any Linux host; --no-tailscale-ssh + manual steps for other OSes).
#   - Tailscale installed and the host logged into your tailnet
#     (run `tailscale up` once before this script).
#   - sudo (for apt + `tailscale set`).
#
# Requires Tailscale v1.36+ (released Jan 2023) for the `tailscale set --ssh`
# syntax. Older versions: substitute `sudo tailscale up --ssh` and re-up.

set -euo pipefail

# ---------------------------------------------------------------- args

INBOX_DIR="${HOME}/projects/inbox"
ENABLE_TS_SSH=1
DRY_RUN=0

while [ $# -gt 0 ]; do
    case "$1" in
        --inbox-dir)
            INBOX_DIR="$2"
            shift 2
            ;;
        --no-tailscale-ssh)
            ENABLE_TS_SSH=0
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            sed -n '2,40p' "$0"
            exit 0
            ;;
        *)
            echo "unknown arg: $1" >&2
            echo "see: bash $0 --help" >&2
            exit 2
            ;;
    esac
done

run() {
    if [ "$DRY_RUN" = "1" ]; then
        echo "[dry-run] $*"
    else
        echo "[run]    $*"
        "$@"
    fi
}

note() { echo "[note]   $*"; }
warn() { echo "[warn]   $*" >&2; }
fail() { echo "[FAIL]   $*" >&2; exit 1; }

# ---------------------------------------------------------------- 1. WSL check

if ! grep -qiE '(microsoft|wsl)' /proc/version 2>/dev/null; then
    fail "not running under WSL. This script targets WSL specifically; on a non-WSL Linux host, the apt + tailscale + mkdir steps work the same — run them by hand."
fi
note "WSL detected"

# ---------------------------------------------------------------- 2. inbox dir heads-up

if [ -d "$INBOX_DIR" ] && ls "$INBOX_DIR"/*.md >/dev/null 2>&1; then
    warn "$INBOX_DIR already contains .md files — looks like an existing swarm inbox."
    warn "If you want an ISOLATED test swarm for proving remote-channels, re-run with:"
    warn "    bash $0 --inbox-dir ~/projects/inbox-remote-test"
    warn "If you intend to extend that existing swarm to be remote-aware, you're fine —"
    warn "the new remote files won't collide (separate filenames per slice)."
fi

# ---------------------------------------------------------------- 3. apt packages

if ! command -v rsync >/dev/null 2>&1; then
    note "installing rsync"
    run sudo apt-get update -qq
    run sudo apt-get install -y rsync
else
    note "rsync already installed"
fi

# ---------------------------------------------------------------- 4. tailscale presence

if ! command -v tailscale >/dev/null 2>&1; then
    fail "tailscale not found. Install first:
       curl -fsSL https://tailscale.com/install.sh | sh
       sudo tailscale up
    then re-run this script."
fi

if ! tailscale status >/dev/null 2>&1; then
    fail "tailscale is installed but not logged in / daemon not running. Try:
       sudo tailscale up
    then re-run this script."
fi
note "tailscale present and logged in"

# ---------------------------------------------------------------- 5. enable Tailscale SSH

if [ "$ENABLE_TS_SSH" = "1" ]; then
    # Check current state: `tailscale status --json` lists `RunningSSHServer` per peer.
    # For self, we check the local config. The cheap reliable check is to look
    # at `tailscale debug prefs` if available, but simpler: just call `set --ssh`
    # idempotently — it's a no-op if already enabled.
    note "enabling Tailscale SSH (idempotent)"
    run sudo tailscale set --ssh
else
    note "skipping Tailscale SSH enable (--no-tailscale-ssh)"
fi

# ---------------------------------------------------------------- 6. inbox dir

if [ ! -d "$INBOX_DIR" ]; then
    note "creating inbox dir at $INBOX_DIR"
    run mkdir -p "$INBOX_DIR"
else
    note "inbox dir exists at $INBOX_DIR"
fi

# ---------------------------------------------------------------- 7. report

# Get this host's tailnet hostname. `tailscale status --self` may not exist
# on all versions; fall back to parsing `tailscale status` first line.
TS_NAME=""
if tailscale status --self --json >/dev/null 2>&1; then
    TS_NAME=$(tailscale status --self --json 2>/dev/null \
        | grep -o '"DNSName":[[:space:]]*"[^"]*"' \
        | head -1 \
        | sed 's/.*"\([^"]*\)"$/\1/' \
        | sed 's/\.$//')
fi
if [ -z "$TS_NAME" ]; then
    TS_NAME="<run 'tailscale status' to find this host's tailnet hostname>"
fi

cat <<EOF

================================================================================
giga-harness remote-peer setup complete on $(hostname).

This host:
  inbox dir:           $INBOX_DIR
  os user:             $USER
  tailnet hostname:    $TS_NAME

NEXT STEPS (manual, one-time):

1. Repeat this script on every other host that will join the swarm.

2. Verify cross-host rsync works. From THIS host, pick any peer <PEER> and:

       ssh $USER@<PEER> 'echo hello from \$(hostname)'
       rsync -avz /tmp/test.txt $USER@<PEER>:/tmp/

   ...replacing <PEER> with that peer's tailnet hostname (the one printed
   when you ran this script on it). Both should work with NO key prompts —
   Tailscale SSH handles auth via your tailnet identity.

3. Install the giga binary on each host (once the remote-channels feature
   ships). For now, the install.sh at the giga-harness repo root installs
   the current release; you'll re-install once the new release is out.

4. Edit giga-harness.toml on each host. The new [[hosts]] section + per-host
   TOML slices are documented in REMOTE_DESIGN.md (workdir of the giga
   agent). DO NOT edit the TOML yet — wait until the feature has shipped;
   today's giga binary doesn't understand the new schema.

5. Once shipped: a small bootstrap helper (\`giga setup-remote --peer <host>\`)
   will scaffold the host slice TOML for you. Until then, watch the
   release notes for "remote channels" landing.

================================================================================
EOF

if [ "$DRY_RUN" = "1" ]; then
    note "this was a dry-run; no changes were made"
fi
