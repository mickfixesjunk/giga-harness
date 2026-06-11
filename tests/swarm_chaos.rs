//! Swarm-chaos tests — simulate N concurrent agent-shaped writers + a
//! watcher, then assert correctness invariants at the end. Per the test
//! plan: build first, then triage findings.
//!
//! Existing test coverage runs everything single-threaded; the watcher's
//! `len > last_size` invariant + `OpenOptions::append(true)` atomicity
//! only get stressed under real concurrency. These tests fill that gap.
//!
//! All tests are LOCAL-MODE only — no [[hosts]], no sync, no merger.
//! Remote-mode chaos tests are documented in REMOTE_DESIGN.md follow-ups
//! and deferred until the local tier is in.
//!
//! Each test uses its own tempdir + `giga` binary subprocess (so we're
//! exercising the same paths real agents hit). Tempdir + per-test HOME
//! isolation keeps test cursors / busy-locks from cross-contaminating.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const GIGA: &str = env!("CARGO_BIN_EXE_giga");

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct LocalFixture {
    _tmp: TempDir,
    home: PathBuf,
    config_path: PathBuf,
    inbox: PathBuf,
}

impl LocalFixture {
    fn channel_path(&self, file: &str) -> PathBuf {
        self.inbox.join(file)
    }
}

/// Two-agent local swarm: alice + bob + one bilateral. No [[hosts]] —
/// today's local-only mode (the path that 99% of swarms use today).
fn simple_local_swarm() -> LocalFixture {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let swarm_dir = tmp.path().join("swarm");
    let inbox = tmp.path().join("inbox");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&swarm_dir).unwrap();
    fs::create_dir_all(&inbox).unwrap();
    let config_path = swarm_dir.join("giga-harness.toml");
    let toml = format!(
        r#"
[project]
name = "chaos"

[paths]
wsl_inbox = '{inbox}'

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

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
        inbox = inbox.to_string_lossy(),
    );
    fs::write(&config_path, toml).unwrap();
    LocalFixture {
        _tmp: tmp,
        home,
        config_path,
        inbox,
    }
}

// ---------------------------------------------------------------------------
// Subprocess helpers
// ---------------------------------------------------------------------------

/// Drive a `giga post` subprocess. Returns Result so the caller can
/// distinguish "intentional race-loser" from "test infrastructure broke."
fn giga_post(
    home: &Path,
    config: &Path,
    sender: &str,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    let out = Command::new(GIGA)
        .env("HOME", home)
        .args([
            "post",
            "alice-bob.md",
            "--as",
            sender,
            "--subject",
            subject,
            "--body",
            body,
            "--config",
            config.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("spawn giga post: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "giga post --as {sender} --subject {subject} exited {}: stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Header parser — extracts (sender, subject) pairs in file-order. Uses the
// same predicate as src/watch.rs::is_header_line: a line matches if it
// starts with `[`, contains `] `, and ends with an ASCII ISO-8601 timestamp.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Header {
    sender: String,
    subject: String,
}

fn is_post_header(line: &str) -> bool {
    if !line.starts_with('[') || !line.contains("] ") || line.starts_with("[<") {
        return false;
    }
    let bytes = line.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    let tail = &bytes[bytes.len() - 20..];
    tail[19] == b'Z'
        && tail[4] == b'-'
        && tail[7] == b'-'
        && tail[10] == b'T'
        && tail[13] == b':'
        && tail[16] == b':'
}

fn parse_headers(text: &str) -> Vec<Header> {
    text.lines()
        .filter(|l| is_post_header(l))
        .filter_map(|l| {
            // [sender] subject — YYYY-MM-DDTHH:MM:SSZ
            // Use rsplit_once on the " — " separator — char-safe (the
            // em-dash is 3 bytes, so byte-index math from the right end
            // panics on char boundaries; rsplit_once does the right thing).
            let close = l.find("] ")?;
            let sender = l[1..close].to_string();
            let rest = &l[close + 2..];
            let (subject, _ts) = rest.rsplit_once(" — ")?;
            Some(Header {
                sender,
                subject: subject.to_string(),
            })
        })
        .collect()
}

/// Count the number of complete blocks in a channel file: each block has
/// exactly 3 `===` delimiter lines (open, header-close, footer-close).
/// Note: the file may have an initial channel-header block from `giga init`
/// (also 3 `===` lines), so this isn't strict equality with post count —
/// use parse_headers().len() for that.
#[allow(dead_code)]
fn count_delimiter_triples(text: &str) -> usize {
    text.lines().filter(|l| l.trim() == "===").count() / 3
}

// ===========================================================================
// TEST 1 — no clobbering under concurrent post
// ===========================================================================

#[test]
fn local_no_clobbering_under_concurrent_post() {
    let fx = simple_local_swarm();
    let n_per_agent: usize = 100;

    let cfg = Arc::new(fx.config_path.clone());
    let home = Arc::new(fx.home.clone());

    let spawn_agent = |sender: &'static str| {
        let cfg = Arc::clone(&cfg);
        let home = Arc::clone(&home);
        thread::spawn(move || {
            let mut errors = Vec::new();
            for i in 0..n_per_agent {
                let subject = format!("{sender}-msg-{i:03}");
                let body = format!("body from {sender} #{i}");
                if let Err(e) = giga_post(&home, &cfg, sender, &subject, &body) {
                    errors.push(format!("{sender} #{i}: {e}"));
                }
            }
            errors
        })
    };

    let h_alice = spawn_agent("alice");
    let h_bob = spawn_agent("bob");

    let alice_errors = h_alice.join().expect("alice thread panicked");
    let bob_errors = h_bob.join().expect("bob thread panicked");
    assert!(
        alice_errors.is_empty() && bob_errors.is_empty(),
        "post errors: alice={alice_errors:?}, bob={bob_errors:?}",
    );

    let body = fs::read_to_string(fx.channel_path("alice-bob.md")).unwrap();
    let headers = parse_headers(&body);

    assert_eq!(
        headers.len(),
        2 * n_per_agent,
        "expected {} headers (alice {} + bob {}), got {}",
        2 * n_per_agent,
        n_per_agent,
        n_per_agent,
        headers.len(),
    );

    // Per-author ordering: filter to each author, assert the subjects
    // form the expected sequence in file-order (cross-author interleaving
    // is fine — only per-author monotonicity matters).
    for sender in ["alice", "bob"] {
        let mine: Vec<&Header> = headers.iter().filter(|h| h.sender == sender).collect();
        assert_eq!(
            mine.len(),
            n_per_agent,
            "expected {n_per_agent} headers from {sender}, got {}",
            mine.len(),
        );
        for (i, h) in mine.iter().enumerate() {
            let expected = format!("{sender}-msg-{i:03}");
            assert_eq!(
                h.subject, expected,
                "out-of-order: {sender} block #{i} subject is {:?}, expected {expected}",
                h.subject,
            );
        }
    }
}

// ===========================================================================
// TEST 2 — watcher fires for every concurrent post + self-filters
// ===========================================================================

#[test]
fn local_watcher_fires_for_every_concurrent_post() {
    let fx = simple_local_swarm();
    let n: usize = 10;

    // Spawn `giga watch --as alice` in the background. It tails the channel;
    // each notification appears on its stdout.
    let log_path = fx._tmp.path().join("watch-alice.log");
    let log = fs::File::create(&log_path).unwrap();
    let mut watcher = Command::new(GIGA)
        .env("HOME", &fx.home)
        .args([
            "watch",
            "--as",
            "alice",
            "--config",
            fx.config_path.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn giga watch");

    // Give the watcher a moment to start + read the initial file (so it
    // doesn't catch our posts as "history replay" — those would all flush
    // at startup which is also valid but harder to count by source).
    thread::sleep(Duration::from_millis(500));

    // Spawn N concurrent posts as bob. Also two as alice (which should be
    // self-filtered by the watcher).
    let cfg = Arc::new(fx.config_path.clone());
    let home = Arc::new(fx.home.clone());

    let mut handles = Vec::new();
    for i in 0..n {
        let cfg = Arc::clone(&cfg);
        let home = Arc::clone(&home);
        handles.push(thread::spawn(move || {
            giga_post(&home, &cfg, "bob", &format!("bob-{i:02}"), "x").unwrap();
        }));
    }
    // Alice's own posts — should NOT appear in her watcher's output.
    for i in 0..2 {
        let cfg = Arc::clone(&cfg);
        let home = Arc::clone(&home);
        handles.push(thread::spawn(move || {
            giga_post(&home, &cfg, "alice", &format!("self-{i:02}"), "x").unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // Wait long enough for the watcher's poll loop (3s tick) to pick up
    // every post + flush. 8s is comfortably more than 2 ticks.
    thread::sleep(Duration::from_secs(8));

    // Send watcher SIGTERM + clean up.
    let _ = watcher.kill();
    let _ = watcher.wait();

    let log_body = fs::read_to_string(&log_path).unwrap();
    // Each notification line looks like:
    //   inbox alice-bob.md: [bob] bob-00 — 2026-...
    let bob_notifs = log_body
        .lines()
        .filter(|l| l.contains("[bob] bob-"))
        .count();
    let alice_notifs = log_body
        .lines()
        .filter(|l| l.contains("[alice] self-"))
        .count();

    assert_eq!(
        bob_notifs, n,
        "expected {n} notifications for bob's posts, got {bob_notifs}\nlog:\n{log_body}",
    );
    assert_eq!(
        alice_notifs, 0,
        "alice's own posts must be self-filtered; got {alice_notifs} notifications\nlog:\n{log_body}",
    );
}

// ===========================================================================
// TEST 3 — cursor persists across watcher restart (no re-notification)
// ===========================================================================

#[test]
fn local_watcher_cursor_persists_across_restart() {
    let fx = simple_local_swarm();

    // First round: post 3 messages, run watcher long enough to consume.
    for i in 0..3 {
        giga_post(&fx.home, &fx.config_path, "bob", &format!("first-{i}"), "x").unwrap();
    }
    let log1 = fx._tmp.path().join("watch1.log");
    let w1_handle = spawn_watcher(&fx, &log1);
    thread::sleep(Duration::from_secs(7)); // 2+ ticks to flush
    stop_watcher(w1_handle);

    let log1_body = fs::read_to_string(&log1).unwrap();
    let log1_count = log1_body
        .lines()
        .filter(|l| l.contains("[bob] first-"))
        .count();
    assert_eq!(
        log1_count, 3,
        "first watcher should see 3 posts; got {log1_count}\nlog:\n{log1_body}"
    );

    // Second round: post 2 NEW messages, restart watcher. It should
    // resume from the persisted cursor and NOT re-deliver the first 3.
    for i in 0..2 {
        giga_post(
            &fx.home,
            &fx.config_path,
            "bob",
            &format!("second-{i}"),
            "x",
        )
        .unwrap();
    }
    let log2 = fx._tmp.path().join("watch2.log");
    let w2_handle = spawn_watcher(&fx, &log2);
    thread::sleep(Duration::from_secs(7));
    stop_watcher(w2_handle);

    let log2_body = fs::read_to_string(&log2).unwrap();
    let resurfaced_first = log2_body
        .lines()
        .filter(|l| l.contains("[bob] first-"))
        .count();
    let new_second = log2_body
        .lines()
        .filter(|l| l.contains("[bob] second-"))
        .count();
    assert_eq!(
        resurfaced_first, 0,
        "restarted watcher must NOT re-deliver posts the previous watcher already emitted (cursor should advance + persist); got {resurfaced_first} re-deliveries\nlog:\n{log2_body}",
    );
    assert_eq!(
        new_second, 2,
        "restarted watcher must see the 2 new posts; got {new_second}\nlog:\n{log2_body}",
    );
}

fn spawn_watcher(fx: &LocalFixture, log_path: &Path) -> std::process::Child {
    let log = fs::File::create(log_path).unwrap();
    let child = Command::new(GIGA)
        .env("HOME", &fx.home)
        .args([
            "watch",
            "--as",
            "alice",
            "--config",
            fx.config_path.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn giga watch");
    thread::sleep(Duration::from_millis(400)); // let it open the file
    child
}

fn stop_watcher(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

// ===========================================================================
// TEST 4 — atomicity at + above PIPE_BUF (regression guard for future code
// that might split a single block into multiple write_all calls)
// ===========================================================================

#[test]
fn local_post_atomicity_around_pipe_buf_boundary() {
    let fx = simple_local_swarm();

    // POSIX PIPE_BUF is typically 4096 on Linux. Test bodies that, after
    // the header + footer envelope (~150 bytes), push the whole block well
    // ABOVE PIPE_BUF so any non-atomic write_all would show up as
    // interleaving when two are racing.
    let big_body_size = 8 * 1024; // 8KB body → ~8.2KB total block
    let body_a = "A".repeat(big_body_size);
    let body_b = "B".repeat(big_body_size);

    // Two concurrent threads, each posting its big body 5x. We're looking
    // for: are the As ever interleaved with Bs WITHIN a single body?
    let cfg = Arc::new(fx.config_path.clone());
    let home = Arc::new(fx.home.clone());
    let n_iter = 5;

    let body_a = Arc::new(body_a);
    let body_b = Arc::new(body_b);

    let ha = {
        let cfg = Arc::clone(&cfg);
        let home = Arc::clone(&home);
        let body = Arc::clone(&body_a);
        thread::spawn(move || {
            for i in 0..n_iter {
                giga_post(&home, &cfg, "alice", &format!("a-{i}"), &body).unwrap();
            }
        })
    };
    let hb = {
        let cfg = Arc::clone(&cfg);
        let home = Arc::clone(&home);
        let body = Arc::clone(&body_b);
        thread::spawn(move || {
            for i in 0..n_iter {
                giga_post(&home, &cfg, "bob", &format!("b-{i}"), &body).unwrap();
            }
        })
    };
    ha.join().unwrap();
    hb.join().unwrap();

    let text = fs::read_to_string(fx.channel_path("alice-bob.md")).unwrap();

    // Each block's body should be either all-A or all-B (the chars from
    // the OTHER author should never appear in this author's body region).
    // Use the parse_headers output to know there are 2*n_iter blocks; then
    // scan each block's body characters by splitting on '===' separators.
    let headers = parse_headers(&text);
    assert_eq!(
        headers.len(),
        2 * n_iter,
        "expected {} headers, got {}: {headers:?}",
        2 * n_iter,
        headers.len(),
    );

    // Split into chunks at the block-end '===' markers. Crude but
    // sufficient: find each header line, then the next 8K chars should
    // contain only that author's body character (plus whitespace).
    for h in &headers {
        let needle = format!("[{}] {}", h.sender, h.subject);
        let start = text.find(&needle).unwrap_or_else(|| {
            panic!("couldn't relocate header {needle}");
        });
        // The body starts after the header line + "\n===\n\n".
        let body_start = start
            + needle.len()
            + " — 2026-01-01T00:00:00Z".len() // length of timestamp tail
            + "\n===\n\n".len();
        let expected_char = if h.sender == "alice" { 'A' } else { 'B' };
        let other_char = if h.sender == "alice" { 'B' } else { 'A' };
        // Read the first 200 chars of body — if interleaving happened
        // this is where we'd see it.
        let sample = &text[body_start..body_start.saturating_add(200).min(text.len())];
        assert!(
            !sample.contains(other_char),
            "interleaved bytes! header={needle}, body sample contains '{other_char}':\nsample={sample:?}",
        );
        assert!(
            sample.contains(expected_char),
            "expected '{expected_char}' in body of {needle}, sample={sample:?}",
        );
    }
}

// ===========================================================================
// TEST 5 — `giga sweep` must not crash while agents are actively posting
// ===========================================================================

#[test]
fn local_sweep_under_active_traffic() {
    let fx = simple_local_swarm();

    // Background traffic: alice + bob each post 30 messages with a small
    // sleep between to keep the channel actively growing for several
    // seconds.
    let cfg = Arc::new(fx.config_path.clone());
    let home = Arc::new(fx.home.clone());
    let n_per_agent = 30;

    let traffic_handle = {
        let cfg = Arc::clone(&cfg);
        let home = Arc::clone(&home);
        thread::spawn(move || {
            let mut handles = Vec::new();
            for sender in ["alice", "bob"] {
                let cfg = Arc::clone(&cfg);
                let home = Arc::clone(&home);
                handles.push(thread::spawn(move || {
                    for i in 0..n_per_agent {
                        let _ = giga_post(&home, &cfg, sender, &format!("{sender}-t-{i:02}"), "x");
                        thread::sleep(Duration::from_millis(50));
                    }
                }));
            }
            for h in handles {
                h.join().unwrap();
            }
        })
    };

    // Hammer `giga sweep` repeatedly while traffic is flowing. Looking
    // for: any sweep call panics, exits non-zero, or produces garbled
    // output that can't be read.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut sweep_runs = 0;
    let mut sweep_failures = Vec::new();
    while Instant::now() < deadline {
        sweep_runs += 1;
        let out = Command::new(GIGA)
            .env("HOME", &fx.home)
            .args(["sweep", fx.config_path.to_str().unwrap()])
            .output()
            .expect("spawn giga sweep");
        if !out.status.success() {
            sweep_failures.push(format!(
                "sweep #{sweep_runs} exited {}: stderr={}",
                out.status,
                String::from_utf8_lossy(&out.stderr),
            ));
        }
        thread::sleep(Duration::from_millis(150));
    }
    traffic_handle.join().unwrap();

    assert!(
        sweep_runs >= 10,
        "should have run sweep at least 10 times in 3s; ran {sweep_runs}",
    );
    assert!(
        sweep_failures.is_empty(),
        "{} of {sweep_runs} sweeps failed:\n{}",
        sweep_failures.len(),
        sweep_failures.join("\n"),
    );

    // Final assertion: after all traffic, the channel should have exactly
    // 2 * n_per_agent valid headers — sweep activity shouldn't have
    // corrupted the file.
    let final_text = fs::read_to_string(fx.channel_path("alice-bob.md")).unwrap();
    let headers = parse_headers(&final_text);
    assert_eq!(
        headers.len(),
        2 * n_per_agent,
        "final channel should contain all {} posts; got {} headers\nfile:\n{}",
        2 * n_per_agent,
        headers.len(),
        final_text,
    );
}
