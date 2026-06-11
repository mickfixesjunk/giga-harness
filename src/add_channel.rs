//! `giga add-channel --participants alice,bob` — append a new
//! bilateral channel to the canonical TOML.
//!
//! When the operator wants to connect an existing
//! local agent to an existing remote agent via a new bilateral, this
//! is the subcommand. It's a TOML edit; the `giga sync` daemon (step 5)
//! propagates the updated TOML to peers; the merger + watcher pick up
//! the new channel within ~15s (the auto-discovery reload window).
//!
//! v1 supports bilateral (2-participant) channels only. Multi-party
//! / broadcast channels are still created by hand in TOML or via
//! `giga add-agent --peer A --peer B --peer C` adding bilaterals
//! per peer.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use toml_edit::DocumentMut;

use crate::add_agent::{append_channel, DerivedChannel};
use crate::config::Config;

pub struct Args {
    pub config: PathBuf,
    pub participants: Vec<String>,
    /// Override the auto-derived filename (`alice-bob.md`). Rarely needed.
    pub file: Option<String>,
    /// Print the planned change; don't write.
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let cfg = Config::load(&args.config)?;
    let ch = derive(&cfg, &args)?;

    // Idempotency: if a channel with this filename already exists, refuse
    // (loud rather than silently no-op so user catches collision).
    if cfg.channels.iter().any(|c| c.file == ch.file) {
        return Err(anyhow!(
            "channel `{}` already exists in {} — nothing to do",
            ch.file,
            args.config.display(),
        ));
    }

    if args.dry_run {
        println!("dry-run: would add channel");
        println!("  file:         {}", ch.file);
        println!("  side:         {}", ch.side);
        println!(
            "  participants: {} <-> {}",
            ch.participants[0], ch.participants[1]
        );
        return Ok(());
    }

    // Edit the TOML doc preserving comments + formatting.
    let original = fs::read_to_string(&args.config)
        .with_context(|| format!("reading {}", args.config.display()))?;
    let mut doc: DocumentMut = original
        .parse()
        .with_context(|| format!("parsing {} as TOML", args.config.display()))?;
    append_channel(&mut doc, &ch)?;
    fs::write(&args.config, doc.to_string())
        .with_context(|| format!("writing {}", args.config.display()))?;

    // Re-validate against the parsed config — catches "channels reference
    // unknown agents" + the new validation rules from step 1.
    Config::load(&args.config).with_context(|| {
        format!(
            "added channel `{}` but post-edit validation failed",
            ch.file
        )
    })?;

    println!("added channel `{}` to {}", ch.file, args.config.display());
    if cfg.hosts.is_empty() {
        println!("(local-only swarm — no sync needed)");
    } else {
        println!("(cross-host swarm — run `giga sync --once` if you want to push the TOML to peers immediately, otherwise sync picks it up next tick)");
    }
    Ok(())
}

/// Derive the channel record from CLI args + the parsed config.
/// Pure — testable.
pub(crate) fn derive(cfg: &Config, args: &Args) -> Result<DerivedChannel> {
    if args.participants.len() != 2 {
        return Err(anyhow!(
            "v1 supports bilateral channels only — got {} participants",
            args.participants.len(),
        ));
    }
    let a_name = &args.participants[0];
    let b_name = &args.participants[1];

    let a = cfg
        .agents
        .iter()
        .find(|x| &x.name == a_name)
        .ok_or_else(|| anyhow!("participant `{a_name}` isn't in [[agents]]"))?;
    let b = cfg
        .agents
        .iter()
        .find(|x| &x.name == b_name)
        .ok_or_else(|| anyhow!("participant `{b_name}` isn't in [[agents]]"))?;

    // Sorted-alphabetical filename — matches add_agent's convention.
    let mut both = vec![a_name.clone(), b_name.clone()];
    both.sort();
    let auto_file = format!("{}-{}.md", both[0], both[1]);
    let file = args.file.clone().unwrap_or(auto_file);

    // Side: if either participant is windows-platform, channel lives on
    // the windows side so the native Windows agent can reach it.
    let side = if a.platform == "windows" || b.platform == "windows" {
        "windows"
    } else {
        "wsl"
    }
    .to_string();

    Ok(DerivedChannel {
        file,
        side,
        participants: [both[0].clone(), both[1].clone()],
        purpose: format!("Bilateral channel between {} and {}.", both[0], both[1]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_cfg(tmp: &TempDir, body: &str) -> PathBuf {
        let p = tmp.path().join("giga-harness.toml");
        fs::write(&p, body).unwrap();
        p
    }

    fn base_cfg() -> &'static str {
        r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"

[[agents]]
name = "winbob"
workdir = "C:\\Users\\b"
role = "."
platform = "windows"
"#
    }

    #[test]
    fn derive_sorts_filename_alphabetically() {
        let tmp = TempDir::new().unwrap();
        let path = write_cfg(&tmp, base_cfg());
        let cfg = Config::load(&path).unwrap();
        let args = Args {
            config: path,
            participants: vec!["bob".into(), "alice".into()],
            file: None,
            dry_run: true,
        };
        let ch = derive(&cfg, &args).unwrap();
        assert_eq!(ch.file, "alice-bob.md");
        assert_eq!(ch.participants, ["alice".to_string(), "bob".to_string()]);
    }

    #[test]
    fn derive_picks_windows_side_when_either_participant_is_windows() {
        let tmp = TempDir::new().unwrap();
        let path = write_cfg(&tmp, base_cfg());
        let cfg = Config::load(&path).unwrap();
        let args = Args {
            config: path,
            participants: vec!["alice".into(), "winbob".into()],
            file: None,
            dry_run: true,
        };
        let ch = derive(&cfg, &args).unwrap();
        assert_eq!(ch.side, "windows");
    }

    #[test]
    fn derive_rejects_unknown_participant() {
        let tmp = TempDir::new().unwrap();
        let path = write_cfg(&tmp, base_cfg());
        let cfg = Config::load(&path).unwrap();
        let args = Args {
            config: path,
            participants: vec!["alice".into(), "ghost".into()],
            file: None,
            dry_run: true,
        };
        let err = derive(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("ghost"));
    }

    #[test]
    fn derive_rejects_wrong_participant_count() {
        let tmp = TempDir::new().unwrap();
        let path = write_cfg(&tmp, base_cfg());
        let cfg = Config::load(&path).unwrap();
        let args = Args {
            config: path,
            participants: vec!["alice".into()],
            file: None,
            dry_run: true,
        };
        let err = derive(&cfg, &args).unwrap_err();
        assert!(err.to_string().contains("bilateral"));
    }

    #[test]
    fn run_appends_channel_and_validates() {
        let tmp = TempDir::new().unwrap();
        let path = write_cfg(&tmp, base_cfg());
        run(Args {
            config: path.clone(),
            participants: vec!["alice".into(), "bob".into()],
            file: None,
            dry_run: false,
        })
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        assert!(cfg.channels.iter().any(|c| c.file == "alice-bob.md"));
    }

    #[test]
    fn run_refuses_duplicate_channel() {
        let tmp = TempDir::new().unwrap();
        let cfg = format!(
            r#"{}
[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            base_cfg()
        );
        let path = write_cfg(&tmp, &cfg);
        let err = run(Args {
            config: path,
            participants: vec!["alice".into(), "bob".into()],
            file: None,
            dry_run: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn run_dry_run_does_not_modify_file() {
        let tmp = TempDir::new().unwrap();
        let path = write_cfg(&tmp, base_cfg());
        let before = fs::read_to_string(&path).unwrap();
        run(Args {
            config: path.clone(),
            participants: vec!["alice".into(), "bob".into()],
            file: None,
            dry_run: true,
        })
        .unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "dry-run must not write");
    }
}
