//! `giga watch <channel> --as <agent>` — built-in inbox watcher.
//!
//! Replaces the bash + powershell watch-channel scripts with a
//! single cross-platform binary. Polls the channel file every 3
//! seconds, prints `inbox: <line>` for every new header block whose
//! sender is NOT `--as`. Runs forever; meant to be invoked under
//! Claude Code's Monitor tool with `persistent: true`.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

pub fn run(channel: &Path, me: &str) -> Result<()> {
    if !channel.exists() {
        anyhow::bail!("channel file not found: {}", channel.display());
    }
    let mut last = fs::metadata(channel)
        .with_context(|| format!("stat {}", channel.display()))?
        .len();
    let me_tag = format!("[{me}] ");
    loop {
        thread::sleep(Duration::from_secs(3));
        let cur = match fs::metadata(channel) {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if cur <= last {
            // Truncation or no growth — reset baseline if truncated
            // (file replaced), skip otherwise.
            if cur < last {
                last = cur;
            }
            continue;
        }
        let delta = match read_delta(channel, last, cur) {
            Ok(d) => d,
            Err(_) => continue,
        };
        last = cur;
        for line in delta.lines() {
            if !line.starts_with('[') {
                continue;
            }
            // Only emit lines that look like header blocks: `[sender] subject — ts`
            if !line.contains("] ") {
                continue;
            }
            if line.starts_with(&me_tag) {
                continue;
            }
            // Stdout, line-buffered. Monitor consumes one event per line.
            println!("inbox: {line}");
        }
    }
}

fn read_delta(path: &Path, from: u64, to: u64) -> Result<String> {
    let mut f = fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0u8; (to - from) as usize];
    f.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}
