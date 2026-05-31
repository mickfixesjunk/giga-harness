//! `giga setup --remote-node` — bootstrap a bare WSL host to join an
//! existing giga-harness swarm as a remote peer.
//!
//! The full UX (per Mick, 2026-05-31):
//!
//!   1. On the NEW node (bare WSL with nothing installed):
//!        curl -fsSL https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh | bash
//!        giga setup --remote-node
//!
//!   2. On the OPERATOR host (already in the swarm):
//!        giga add-agent --host <new-node> --name <agent> --peer <local-agent>
//!
//!   3. Done.
//!
//! This subcommand walks the new node through everything it needs:
//!   - WSL detection (we only support WSL/Linux for v1)
//!   - Install rsync via apt (needed for `giga sync` transport)
//!   - Install Tailscale via the official install.sh (needed for the
//!     SSH-over-tailnet transport)
//!   - Run `tailscale up` (interactive — prints the auth URL the user
//!     visits in a browser to log into their tailnet)
//!   - Enable Tailscale SSH via `tailscale set --ssh` (so the operator
//!     can `giga remote --host <this>` without any keypair exchange)
//!   - Create the default inbox directory
//!   - Print this host's tailnet hostname + the next command the
//!     operator should run from their machine
//!
//! Idempotent — safe to re-run. Each step checks current state and
//! only acts if needed.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

pub struct Args {
    /// Where to create the inbox directory. Defaults to ~/projects/inbox.
    pub inbox_dir: Option<PathBuf>,
    /// Print what would happen without making changes.
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let inbox_dir = match args.inbox_dir {
        Some(p) => p,
        None => default_inbox_dir()?,
    };
    let dry = args.dry_run;

    println!("giga setup --remote-node — bootstrapping this host as a remote peer");
    println!();

    // -------- 1. WSL check --------
    step(1, 6, "WSL detection", dry, || {
        let ver = std::fs::read_to_string("/proc/version")
            .context("reading /proc/version")?;
        if !ver.to_lowercase().contains("microsoft") && !ver.to_lowercase().contains("wsl") {
            return Err(anyhow!(
                "not running under WSL. This subcommand currently targets WSL/Linux; \
                 on macOS the same manual steps apply (brew install tailscale + rsync)."
            ));
        }
        println!("    WSL detected");
        Ok(())
    })?;

    // -------- 2. rsync --------
    step(2, 6, "rsync (for slice file transport)", dry, || {
        if which::which("rsync").is_ok() {
            println!("    already installed");
            return Ok(());
        }
        println!("    installing rsync via apt...");
        run_sudo(&["apt-get", "update", "-qq"])?;
        run_sudo(&["apt-get", "install", "-y", "rsync"])?;
        Ok(())
    })?;

    // -------- 3. Tailscale install --------
    step(3, 6, "Tailscale (for SSH-over-tailnet transport)", dry, || {
        if which::which("tailscale").is_ok() {
            println!("    already installed");
            return Ok(());
        }
        println!("    installing Tailscale via the official install.sh...");
        // Tailscale's installer handles sudo internally + asks no questions.
        let status = Command::new("bash")
            .arg("-c")
            .arg("curl -fsSL https://tailscale.com/install.sh | sh")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("invoking Tailscale install.sh")?;
        if !status.success() {
            return Err(anyhow!(
                "Tailscale install.sh exited {}; see output above",
                status.code().unwrap_or(-1)
            ));
        }
        Ok(())
    })?;

    // -------- 4. tailscale up (interactive) --------
    step(4, 6, "Joining your tailnet (interactive)", dry, || {
        if tailscale_logged_in()? {
            println!("    already logged into your tailnet");
            return Ok(());
        }
        println!("    running `sudo tailscale up` — visit the URL it prints to authorize this node");
        let status = Command::new("sudo")
            .args(["tailscale", "up"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("invoking `sudo tailscale up`")?;
        if !status.success() {
            return Err(anyhow!(
                "`tailscale up` exited {}; you may need to run it manually and re-try",
                status.code().unwrap_or(-1)
            ));
        }
        Ok(())
    })?;

    // -------- 5. Tailscale SSH --------
    step(5, 6, "Enabling Tailscale SSH (no keypair exchange needed)", dry, || {
        run_sudo(&["tailscale", "set", "--ssh"])?;
        println!("    Tailscale SSH enabled");
        Ok(())
    })?;

    // -------- 6. Inbox dir --------
    step(6, 6, &format!("Inbox dir at {}", inbox_dir.display()), dry, || {
        if inbox_dir.exists() {
            // Warn-not-fail: this might be an existing local swarm's
            // inbox. The remote-channels feature won't collide because
            // slice files have distinct suffixes, but flag it so the
            // user knows.
            let has_md = std::fs::read_dir(&inbox_dir)
                .ok()
                .map(|rd| {
                    rd.flatten()
                        .any(|e| e.path().extension().map(|e| e == "md").unwrap_or(false))
                })
                .unwrap_or(false);
            if has_md {
                println!(
                    "    note: {} already contains .md files (existing swarm?). \
                     Remote slice files won't collide (separate filenames per host).",
                    inbox_dir.display()
                );
            } else {
                println!("    already exists");
            }
            return Ok(());
        }
        std::fs::create_dir_all(&inbox_dir)
            .with_context(|| format!("creating {}", inbox_dir.display()))?;
        println!("    created");
        Ok(())
    })?;

    // -------- summary --------
    println!();
    println!("================================================================================");
    println!("Remote node bootstrap complete on {}.", hostname());
    println!();
    let ts_name = tailnet_hostname().unwrap_or_else(|_| "<run `tailscale status` to find it>".into());
    println!("This host's tailnet hostname:  {ts_name}");
    println!("Inbox directory:               {}", inbox_dir.display());
    println!();
    println!("NEXT (from your OPERATOR host — the box where you run giga add-agent):");
    println!();
    println!("  giga add-agent --host <NAME-FOR-THIS-HOST> \\");
    println!("                 --name <AGENT-SLUG> \\");
    println!("                 --peer <EXISTING-LOCAL-AGENT> \\");
    println!("                 --role \"...\"");
    println!();
    println!("Where <NAME-FOR-THIS-HOST> is the slug you'll give this host in [[hosts]].");
    println!("Make sure your operator's giga-harness.toml has a [[hosts]] entry like:");
    println!();
    println!("  [[hosts]]");
    println!("  name = \"<NAME-FOR-THIS-HOST>\"");
    println!("  tailnet_hostname = \"{ts_name}\"");
    println!();
    println!("After that, giga sync (on operator) pushes the canonical TOML here,");
    println!("and giga launch --host <NAME-FOR-THIS-HOST> --only <AGENT-SLUG>");
    println!("brings up the new agent's terminal here.");
    println!("================================================================================");

    if dry {
        println!();
        println!("(dry-run — no changes made)");
    }
    Ok(())
}

/// Helper that prints `[N/M] <label>` and runs the inner action.
/// On dry-run it skips the action but still prints the label.
fn step<F: FnOnce() -> Result<()>>(n: u32, total: u32, label: &str, dry: bool, f: F) -> Result<()> {
    println!("[{n}/{total}] {label}");
    if dry {
        println!("    (dry-run — skipped)");
        return Ok(());
    }
    f().with_context(|| format!("step {n}/{total}: {label}"))
}

/// Run `sudo <args...>` with inherited stdio. Sudo will prompt for a
/// password on first use; that's fine — the user is interactive.
fn run_sudo(args: &[&str]) -> Result<()> {
    let status = Command::new("sudo")
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("invoking `sudo {}`", args.join(" ")))?;
    if !status.success() {
        return Err(anyhow!(
            "`sudo {}` exited {}",
            args.join(" "),
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Detect whether Tailscale is logged in to a tailnet. `tailscale status`
/// returns 0 when logged in, non-zero otherwise (including "Logged out").
fn tailscale_logged_in() -> Result<bool> {
    let out = Command::new("tailscale")
        .arg("status")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("invoking `tailscale status`")?;
    Ok(out.status.success())
}

/// Best-effort fetch of this host's tailnet FQDN (e.g.
/// `wsl-box.tail1234.ts.net`). Falls back to a hint string if the
/// command output isn't what we expect.
fn tailnet_hostname() -> Result<String> {
    let out = Command::new("tailscale")
        .args(["status", "--self", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .context("invoking `tailscale status --self --json`")?;
    if !out.status.success() {
        return Err(anyhow!("`tailscale status --self --json` failed"));
    }
    let json = String::from_utf8_lossy(&out.stdout);
    // Avoid pulling in serde_json just for this one parse — grep the
    // DNSName field out by string match. Tailscale's output is
    // structured + stable enough that this is fine for v1.
    let name = json
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("\"DNSName\":")
                .and_then(|s| s.trim_start().strip_prefix("\""))
                .and_then(|s| s.split('"').next())
        })
        .ok_or_else(|| anyhow!("DNSName not found in tailscale status output"))?
        .trim_end_matches('.')
        .to_string();
    Ok(name)
}

fn default_inbox_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow!("$HOME not set — pass --inbox-dir explicitly"))?;
    Ok(PathBuf::from(home).join("projects").join("inbox"))
}

fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "<unknown>".into())
}
