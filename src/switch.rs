//! `giga switch --runtime claude` — flip which account credentials
//! file is active in `~/.claude/.credentials.json`.
//!
//! Design uses real files only (no symlinks): claude's `/login` and
//! silent OAuth token refreshes both use write-temp-then-rename,
//! which destroys symlinks. Instead we keep a snapshot per account in
//! `~/.claude-accounts/<name>.json` plus a `.active` marker, and copy
//! the appropriate snapshot into `~/.claude/.credentials.json` on
//! switch (saving the previously-active one back first so we don't
//! lose any in-place token refreshes).
//!
//! `mcpOAuth` (third-party MCP tokens) lives inside the same file —
//! it travels with the account on every switch. If you only log MCPs
//! into one account, those tokens go missing when the other is
//! active. Today that's per-account by design; cross-account MCP
//! token sharing would need its own subcommand.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Filesystem layout for the claude runtime. Decoupled from
/// `dirs::home_dir()` so tests can point it at a TempDir.
pub struct ClaudePaths {
    pub home: PathBuf,
}

impl ClaudePaths {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME not set")?;
        Ok(Self { home })
    }

    pub fn cred_file(&self) -> PathBuf {
        self.home.join(".claude/.credentials.json")
    }
    pub fn accounts_dir(&self) -> PathBuf {
        self.home.join(".claude-accounts")
    }
    pub fn active_marker(&self) -> PathBuf {
        self.accounts_dir().join(".active")
    }
    pub fn account_file(&self, name: &str) -> PathBuf {
        self.accounts_dir().join(format!("{name}.json"))
    }
}

pub struct Args {
    pub runtime: String,
    pub account: Option<String>,
    pub op: Op,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Bare invocation: show current account + list known.
    Status,
    /// `--list` — list known accounts.
    List,
    /// `--setup` — adopt the existing `~/.claude/.credentials.json`
    /// as a named snapshot. One-time bootstrap.
    Setup,
    /// `--add` — provision an empty slot. Populate by switching to it
    /// then running `claude` and going through `/login`.
    Add,
    /// Positional `<account>` only — switch to that account.
    Switch,
}

pub fn run(args: Args) -> Result<()> {
    if args.runtime != "claude" {
        bail!(
            "unsupported --runtime `{}`. only `claude` is supported today",
            args.runtime
        );
    }
    let paths = ClaudePaths::from_env()?;
    match args.op {
        Op::Status => op_status(&paths),
        Op::List => op_list(&paths),
        Op::Setup => op_setup(&paths, account_required(&args, "--setup")?),
        Op::Add => op_add(&paths, account_required(&args, "--add")?),
        Op::Switch => op_switch(&paths, account_required(&args, "<account>")?),
    }
}

fn account_required<'a>(args: &'a Args, flag: &str) -> Result<&'a str> {
    args.account
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires an account name"))
}

fn op_status(paths: &ClaudePaths) -> Result<()> {
    let active = read_active(paths)?;
    match active {
        Some(name) => println!("current: {name}"),
        None => println!("current: (none — run `giga switch --runtime claude --setup <name>`)"),
    }
    println!();
    println!("known accounts:");
    print_account_list(paths)?;
    Ok(())
}

fn op_list(paths: &ClaudePaths) -> Result<()> {
    print_account_list(paths)
}

fn op_setup(paths: &ClaudePaths, name: &str) -> Result<()> {
    validate_name(name)?;
    let cred = paths.cred_file();
    if !cred.exists() {
        bail!(
            "no credentials at {} — run `claude` and log in first",
            cred.display()
        );
    }
    if paths.active_marker().exists() {
        let current = read_active(paths)?.unwrap_or_else(|| "(unknown)".into());
        bail!(
            "already set up — active account is `{current}`. \
             To add another, use --add."
        );
    }

    fs::create_dir_all(paths.accounts_dir())
        .with_context(|| format!("creating {}", paths.accounts_dir().display()))?;
    set_mode(&paths.accounts_dir(), 0o700)?;

    let target = paths.account_file(name);
    if target.exists() {
        bail!(
            "{} already exists — pick another name or remove the file",
            target.display()
        );
    }

    copy_cred_file(&cred, &target)?;
    write_active(paths, name)?;

    println!("ok — adopted current credentials as `{name}` and made it active");
    println!("  snapshot: {}", target.display());
    println!("  active marker: {}", paths.active_marker().display());
    println!();
    println!("next: add an overflow account with");
    println!("  giga switch --runtime claude --add <name>");
    Ok(())
}

fn op_add(paths: &ClaudePaths, name: &str) -> Result<()> {
    validate_name(name)?;
    require_setup(paths)?;

    let target = paths.account_file(name);
    if target.exists() {
        bail!("account `{name}` already exists at {}", target.display());
    }

    write_account_placeholder(&target)?;
    println!("ok — provisioned empty slot for `{name}`");
    println!("  {}", target.display());
    println!();
    println!("to populate:");
    println!("  giga switch --runtime claude {name}     # make active");
    println!("  claude                                  # interactive, go through /login");
    let prev = read_active(paths)?.unwrap_or_else(|| "<previous>".into());
    println!("  giga switch --runtime claude {prev}     # switch back (saves the new tokens)");
    Ok(())
}

fn op_switch(paths: &ClaudePaths, new_name: &str) -> Result<()> {
    validate_name(new_name)?;
    require_setup(paths)?;

    let target = paths.account_file(new_name);
    if !target.exists() {
        bail!(
            "no snapshot for `{new_name}` at {}\n\
             create one with: giga switch --runtime claude --add {new_name}",
            target.display()
        );
    }

    let old_name = read_active(paths)?;
    if old_name.as_deref() == Some(new_name) {
        println!("already on `{new_name}` — nothing to do");
        return Ok(());
    }

    // Snapshot the currently-active credentials BEFORE overwriting,
    // so any in-place token refreshes claude did while running on the
    // old account are preserved into its snapshot.
    let cred = paths.cred_file();
    if let Some(ref old) = old_name {
        if cred.exists() {
            let old_target = paths.account_file(old);
            copy_cred_file(&cred, &old_target)
                .with_context(|| format!("snapshotting `{old}` before switch"))?;
        }
    }

    copy_cred_file(&target, &cred).with_context(|| format!("loading `{new_name}` credentials"))?;
    write_active(paths, new_name)?;

    match old_name {
        Some(old) => println!("switched: {old} -> {new_name}"),
        None => println!("loaded: {new_name}"),
    }
    println!("  active credentials: {}", cred.display());
    println!();
    println!("running claude processes still hold the previous auth in memory.");
    println!("to migrate the swarm:");
    println!("  pkill -f '^claude$'                # OR close the agent tabs");
    println!("  giga launch <config>               # tabs re-spawn as `claude -c`, resume each agent on `{new_name}`");
    Ok(())
}

// ---------- helpers ----------

fn require_setup(paths: &ClaudePaths) -> Result<()> {
    if !paths.accounts_dir().exists() || !paths.active_marker().exists() {
        bail!(
            "not set up — run `giga switch --runtime claude --setup <name>` first \
             (migrates your existing ~/.claude/.credentials.json into a named snapshot)"
        );
    }
    Ok(())
}

fn read_active(paths: &ClaudePaths) -> Result<Option<String>> {
    let marker = paths.active_marker();
    if !marker.exists() {
        return Ok(None);
    }
    let s = fs::read_to_string(&marker)
        .with_context(|| format!("reading {}", marker.display()))?;
    let trimmed = s.trim().to_string();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed))
    }
}

fn write_active(paths: &ClaudePaths, name: &str) -> Result<()> {
    let marker = paths.active_marker();
    fs::write(&marker, format!("{name}\n"))
        .with_context(|| format!("writing {}", marker.display()))?;
    set_mode(&marker, 0o600)?;
    Ok(())
}

fn print_account_list(paths: &ClaudePaths) -> Result<()> {
    let dir = paths.accounts_dir();
    if !dir.exists() {
        println!("(no accounts dir — run --setup)");
        return Ok(());
    }
    let active = read_active(paths)?;
    let mut found = 0;
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|r| r.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "json"))
        .collect();
    entries.sort();
    for p in &entries {
        let Some(name) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        found += 1;
        if active.as_deref() == Some(name) {
            println!("* {name}  (active)");
        } else {
            println!("  {name}");
        }
    }
    if found == 0 {
        println!("(none)");
    }
    Ok(())
}

fn copy_cred_file(from: &Path, to: &Path) -> Result<()> {
    let data = fs::read(from).with_context(|| format!("reading {}", from.display()))?;
    // Write via temp + rename so a crash mid-copy doesn't leave a
    // half-written credentials file (which would brick claude).
    let tmp = to.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(&data)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.sync_all().ok();
    }
    set_mode(&tmp, 0o600)?;
    fs::rename(&tmp, to)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), to.display()))?;
    Ok(())
}

fn write_account_placeholder(path: &Path) -> Result<()> {
    fs::write(path, "{}\n").with_context(|| format!("writing {}", path.display()))?;
    set_mode(path, 0o600)?;
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    let mut perm = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perm.set_mode(mode);
    fs::set_permissions(path, perm)
        .with_context(|| format!("chmod {} -> {:o}", path.display(), mode))?;
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("account name cannot be empty");
    }
    if name.starts_with('.') || name.starts_with('-') {
        bail!("account name cannot start with `.` or `-`");
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        bail!("account name cannot contain `/`, `\\`, or NUL");
    }
    if name == "active" {
        bail!("`active` is reserved");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_home() -> (TempDir, ClaudePaths) {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        (
            TempDir::new_in(tmp.path()).unwrap(), // keep parent alive
            ClaudePaths {
                home: tmp.into_path(),
            },
        )
    }

    fn write_cred(paths: &ClaudePaths, body: &str) {
        let p = paths.cred_file();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, body).unwrap();
    }

    fn args(op: Op, account: Option<&str>) -> Args {
        Args {
            runtime: "claude".into(),
            account: account.map(String::from),
            op,
        }
    }

    #[test]
    fn setup_fails_without_credentials() {
        let (_keep, paths) = fake_home();
        let err = op_setup(&paths, "alice").unwrap_err();
        assert!(err.to_string().contains("no credentials"));
    }

    #[test]
    fn setup_creates_snapshot_and_marker() {
        let (_keep, paths) = fake_home();
        write_cred(&paths, "{\"alice\":1}");
        op_setup(&paths, "alice").unwrap();
        assert_eq!(read_active(&paths).unwrap().unwrap(), "alice");
        assert!(paths.account_file("alice").exists());
        let snap = fs::read_to_string(paths.account_file("alice")).unwrap();
        assert_eq!(snap, "{\"alice\":1}");
    }

    #[test]
    fn setup_refuses_double_setup() {
        let (_keep, paths) = fake_home();
        write_cred(&paths, "{}");
        op_setup(&paths, "alice").unwrap();
        let err = op_setup(&paths, "bob").unwrap_err();
        assert!(err.to_string().contains("already set up"));
    }

    #[test]
    fn add_provisions_empty_slot() {
        let (_keep, paths) = fake_home();
        write_cred(&paths, "{}");
        op_setup(&paths, "alice").unwrap();
        op_add(&paths, "bob").unwrap();
        assert!(paths.account_file("bob").exists());
        assert_eq!(fs::read_to_string(paths.account_file("bob")).unwrap(), "{}\n");
    }

    #[test]
    fn add_refuses_existing_slot() {
        let (_keep, paths) = fake_home();
        write_cred(&paths, "{}");
        op_setup(&paths, "alice").unwrap();
        let err = op_add(&paths, "alice").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn switch_swaps_cred_file_contents() {
        let (_keep, paths) = fake_home();
        write_cred(&paths, "{\"alice\":1}");
        op_setup(&paths, "alice").unwrap();
        op_add(&paths, "bob").unwrap();
        // Pretend bob has real credentials populated.
        fs::write(paths.account_file("bob"), "{\"bob\":2}").unwrap();
        op_switch(&paths, "bob").unwrap();
        assert_eq!(read_active(&paths).unwrap().unwrap(), "bob");
        assert_eq!(fs::read_to_string(paths.cred_file()).unwrap(), "{\"bob\":2}");
    }

    #[test]
    fn switch_snapshots_old_account_first() {
        let (_keep, paths) = fake_home();
        // Setup alice with original tokens.
        write_cred(&paths, "{\"alice-v1\":true}");
        op_setup(&paths, "alice").unwrap();
        op_add(&paths, "bob").unwrap();
        fs::write(paths.account_file("bob"), "{\"bob\":true}").unwrap();
        // Simulate claude doing a silent token refresh on alice while
        // she's active — the live cred file diverges from her snapshot.
        write_cred(&paths, "{\"alice-v2-refreshed\":true}");
        // Switch to bob. Alice's snapshot should capture v2.
        op_switch(&paths, "bob").unwrap();
        let alice_snap = fs::read_to_string(paths.account_file("alice")).unwrap();
        assert_eq!(alice_snap, "{\"alice-v2-refreshed\":true}");
    }

    #[test]
    fn switch_is_noop_when_already_active() {
        let (_keep, paths) = fake_home();
        write_cred(&paths, "{}");
        op_setup(&paths, "alice").unwrap();
        op_switch(&paths, "alice").unwrap();
        assert_eq!(read_active(&paths).unwrap().unwrap(), "alice");
    }

    #[test]
    fn switch_fails_on_unknown_account() {
        let (_keep, paths) = fake_home();
        write_cred(&paths, "{}");
        op_setup(&paths, "alice").unwrap();
        let err = op_switch(&paths, "bob").unwrap_err();
        assert!(err.to_string().contains("no snapshot for `bob`"));
    }

    #[test]
    fn validate_name_rules() {
        assert!(validate_name("alice").is_ok());
        assert!(validate_name("a-b").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name(".hidden").is_err());
        assert!(validate_name("-flag").is_err());
        assert!(validate_name("path/sep").is_err());
        assert!(validate_name("active").is_err());
    }

    #[test]
    fn unsupported_runtime_rejected() {
        let err = run(Args {
            runtime: "codex".into(),
            account: Some("x".into()),
            op: Op::Switch,
        })
        .unwrap_err();
        assert!(err.to_string().contains("unsupported --runtime"));
    }
}
