//! Bridge giga inbox notifications into Codex's filesystem channel.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::Config;
use crate::coordination::cursor;
use crate::foundation::frame;
use crate::foundation::tail::{self, POLL_INTERVAL, RELOAD_EVERY_N_TICKS};

pub(crate) static SEQ: AtomicU64 = AtomicU64::new(0);

pub struct Args {
    pub me: String,
    pub channel_dir: PathBuf,
    pub config: PathBuf,
    pub catch_up: bool,
    pub direct_only: bool,
}

#[derive(Serialize)]
struct Envelope {
    id: String,
    from: String,
    to: String,
    kind: String,
    thread: String,
    swarm: String,
    idempotency_key: String,
    text: String,
    ts: String,
}

struct ChannelState {
    name: String,
    path: PathBuf,
    last_size: u64,
}

pub fn run(args: Args) -> Result<()> {
    if !args.config.exists() {
        anyhow::bail!(
            "config file not found: {} - pass --config <path>",
            args.config.display()
        );
    }

    let inbox_dir = args.channel_dir.join("inbox");
    let outbox_dir = args.channel_dir.join("outbox");
    let processed_dir = args.channel_dir.join("processed");
    fs::create_dir_all(&inbox_dir).with_context(|| format!("creating {}", inbox_dir.display()))?;
    fs::create_dir_all(&outbox_dir)
        .with_context(|| format!("creating {}", outbox_dir.display()))?;
    fs::create_dir_all(&processed_dir)
        .with_context(|| format!("creating {}", processed_dir.display()))?;

    let cfg = Config::load(&args.config)?;
    let project_name = cfg.project.name.clone();
    drop(cfg);

    let giga_home = cursor::giga_home();
    let me_tag = format!("[{}] ", args.me);
    let mut tracked = HashMap::new();
    let mut tick = 0u64;

    refresh_tracked(
        &args.config,
        &args.me,
        &mut tracked,
        giga_home.as_deref(),
        args.catch_up,
        args.direct_only,
    );

    eprintln!(
        "codex-channel: forwarding {} channel(s) for `{}` into {}",
        tracked.len(),
        args.me,
        inbox_dir.display()
    );

    loop {
        thread::sleep(POLL_INTERVAL);
        tick = tick.wrapping_add(1);
        if tick % RELOAD_EVERY_N_TICKS == 0 {
            refresh_tracked(
                &args.config,
                &args.me,
                &mut tracked,
                giga_home.as_deref(),
                args.catch_up,
                args.direct_only,
            );
        }

        for state in tracked.values_mut() {
            let cur = match fs::metadata(&state.path) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            if cur <= state.last_size {
                if cur < state.last_size {
                    state.last_size = cur;
                }
                continue;
            }

            let from = state.last_size;
            let delta = match tail::read_delta_lossy(&state.path, from, cur) {
                Ok(d) => d,
                Err(_) => continue,
            };
            state.last_size = cur;

            for line in delta.lines() {
                if !frame::is_header_line(line) || line.starts_with(&me_tag) {
                    continue;
                }
                let text = format!(
                    "Giga inbox notification for `{me}`.\n\nChannel: {channel}\nPath: {path}\nMessage: {line}\n\nRead the channel file, follow your agent instructions, and respond via `giga post` if the message requires action.",
                    me = args.me,
                    channel = state.name,
                    path = state.path.display(),
                );
                write_envelope(
                    &inbox_dir,
                    &project_name,
                    &args.me,
                    &state.name,
                    from,
                    &text,
                )?;
            }

            if let Some(home) = &giga_home {
                cursor::write(home, &args.me, &state.name, state.last_size);
            }
        }
    }
}

fn refresh_tracked(
    config_path: &Path,
    me: &str,
    tracked: &mut HashMap<String, ChannelState>,
    giga_home: Option<&Path>,
    catch_up: bool,
    direct_only: bool,
) {
    let cfg = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("codex-channel: config reload failed ({e})");
            return;
        }
    };
    let active: Vec<(String, PathBuf)> = cfg
        .channels
        .iter()
        .filter(|c| c.participants.iter().any(|p| p == me))
        .filter(|c| !direct_only || !c.file.starts_with('_'))
        .filter_map(|c| match cfg.channel_path(c) {
            Ok(p) => Some((c.file.clone(), p)),
            Err(e) => {
                eprintln!("codex-channel: skip channel `{}` - {e}", c.file);
                None
            }
        })
        .collect();
    let active_names: HashSet<String> = active.iter().map(|(n, _)| n.clone()).collect();

    for (name, path) in active {
        if tracked.contains_key(&name) {
            continue;
        }
        let eof = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let start = if catch_up {
            giga_home
                .and_then(|home| cursor::read(home, me, &name))
                .unwrap_or(0)
        } else {
            eof
        };
        if start < eof {
            eprintln!(
                "codex-channel: catching up on `{name}` ({} bytes)",
                eof - start
            );
        } else {
            eprintln!("codex-channel: tracking `{name}` at EOF");
        }
        tracked.insert(
            name.clone(),
            ChannelState {
                name,
                path,
                last_size: start,
            },
        );
    }

    let to_drop: Vec<String> = tracked
        .keys()
        .filter(|name| !active_names.contains(*name))
        .cloned()
        .collect();
    for name in to_drop {
        tracked.remove(&name);
        eprintln!("codex-channel: dropped `{name}`");
    }
}

pub(crate) fn write_envelope(
    inbox_dir: &Path,
    swarm: &str,
    me: &str,
    channel: &str,
    offset: u64,
    text: &str,
) -> Result<()> {
    let now = chrono::Utc::now();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let id = format!("giga-{me}-{channel}-{offset}-{seq}");
    let envelope = Envelope {
        id: id.clone(),
        from: "giga".to_string(),
        to: "codex".to_string(),
        kind: "brief".to_string(),
        thread: me.to_string(),
        swarm: swarm.to_string(),
        idempotency_key: id.clone(),
        text: text.to_string(),
        ts: now.to_rfc3339(),
    };
    let bytes = serde_json::to_vec_pretty(&envelope)?;
    let nanos = now.timestamp_nanos_opt().unwrap_or(0);
    let pid = std::process::id();
    let filename = format!("{nanos:020}-{pid:010}-00000000-{seq:010}-from-giga.json");
    let final_path = inbox_dir.join(&filename);
    let tmp_path = inbox_dir.join(format!(".{filename}.tmp"));
    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(&bytes)?;
        f.sync_all().ok();
    }
    fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("publishing {}", final_path.display()))?;
    eprintln!(
        "codex-channel: delivered {channel} -> {}",
        final_path.display()
    );
    Ok(())
}
