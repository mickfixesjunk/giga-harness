//! Cross-host swarm-chaos tests (R1-R3 from the cross-host test
//! plan). Companion to tests/swarm_chaos.rs (local-only) and
//! tests/cross_host_e2e.rs (sequential cross-host).
//!
//! Pattern: simulate 2 hosts via 2 inbox dirs on the local filesystem,
//! fake the rsync transport with `fs::copy`, exercise the post → slice →
//! sync-by-copy → merger → merged pipeline under concurrent load.
//! Each "host" gets its own HOME dir so per-host cursor state stays
//! isolated (matches the cross_host_e2e fixture pattern).
//!
//! NOT TESTED HERE (real rsync requires SSH + a real peer):
//!   - Real `--append-verify` atomicity (rsync's own guarantee)
//!   - Tailscale SSH connection failures
//!   - SSH user auth failures
//! These are covered by the live 2-host smoke (REMOTE_DESIGN.md §6
//! step 10) and by sync.rs unit tests (planner is pure).
//!
//! What we CAN test under chaos: the local slice-and-merge pipeline,
//! merger idempotency under racing slice growth, slice-file readability
//! while it's being appended to (a proxy for rsync's read invariant).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const GIGA: &str = env!("CARGO_BIN_EXE_giga");

// ---------------------------------------------------------------------------
// Fixture: 2 hosts (wsl-a + wsl-b), 2 agents (alice@a + bob@b), 1 bilateral.
// ---------------------------------------------------------------------------

struct CrossHostFixture {
    _tmp: TempDir,
    home_a: PathBuf,
    home_b: PathBuf,
    cfg_a: PathBuf,
    cfg_b: PathBuf,
    inbox_a: PathBuf,
    inbox_b: PathBuf,
}

fn build_fixture() -> CrossHostFixture {
    let tmp = TempDir::new().unwrap();
    let home_a = tmp.path().join("home_a");
    let home_b = tmp.path().join("home_b");
    let host_a = tmp.path().join("host_a");
    let host_b = tmp.path().join("host_b");
    for d in [&home_a, &home_b, &host_a, &host_b] {
        fs::create_dir_all(d).unwrap();
    }
    let cfg_a_dir = host_a.join("swarm");
    let cfg_b_dir = host_b.join("swarm");
    let inbox_a = host_a.join("inbox");
    let inbox_b = host_b.join("inbox");
    for d in [&cfg_a_dir, &cfg_b_dir, &inbox_a, &inbox_b] {
        fs::create_dir_all(d).unwrap();
    }

    let toml_for = |inbox: &Path| -> String {
        format!(
            r#"
[project]
name = "chaos-remote"

[paths]
wsl_inbox = '{inbox}'

[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0.ts.net"

[[agents]]
name = "alice"
workdir = "/h/alice"
role = "."
platform = "wsl"
host = "wsl-a"

[[agents]]
name = "bob"
workdir = "/h/bob"
role = "."
platform = "wsl"
host = "wsl-b"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["alice", "bob"]
"#,
            inbox = inbox.to_string_lossy(),
        )
    };

    let cfg_a = cfg_a_dir.join("giga-harness.toml");
    let cfg_b = cfg_b_dir.join("giga-harness.toml");
    fs::write(&cfg_a, toml_for(&inbox_a)).unwrap();
    fs::write(&cfg_b, toml_for(&inbox_b)).unwrap();
    fs::write(cfg_a_dir.join("this_host.toml"), "this_host = \"wsl-a\"\n").unwrap();
    fs::write(cfg_b_dir.join("this_host.toml"), "this_host = \"wsl-b\"\n").unwrap();

    CrossHostFixture {
        _tmp: tmp,
        home_a,
        home_b,
        cfg_a,
        cfg_b,
        inbox_a,
        inbox_b,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
        .map_err(|e| format!("spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "giga post --as {sender} {subject} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(())
}

fn giga_merger_once(home: &Path, config: &Path) -> Result<(), String> {
    let out = Command::new(GIGA)
        .env("HOME", home)
        .args(["merger", "--once", "--config", config.to_str().unwrap()])
        .output()
        .map_err(|e| format!("spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "giga merger --once exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(())
}

/// Simulate one sync tick by copying each host's OWN slice files to
/// the other host's inbox. Push-only-own (mirrors the real sync
/// invariant from sync.rs::compute_sync_plan): the host owning a
/// slice is its single writer; nobody else gets to send that slice
/// upstream. Without this, host_a's stale snapshot of host_b's slice
/// would get round-tripped back to host_b mid-write and overwrite
/// host_b's actively-growing slice.
fn fake_sync_tick(inbox_a: &Path, inbox_b: &Path) {
    // host_a pushes `.wsl-a.md` slices to host_b.
    push_own_slices(inbox_a, inbox_b, "wsl-a");
    // host_b pushes `.wsl-b.md` slices to host_a.
    push_own_slices(inbox_b, inbox_a, "wsl-b");
}

fn push_own_slices(src: &Path, dst: &Path, own_host: &str) {
    let suffix = format!(".{own_host}.md");
    for entry in fs::read_dir(src).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(&suffix) {
            let _ = fs::copy(entry.path(), dst.join(&name));
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct Header {
    sender: String,
    subject: String,
}

fn parse_headers(text: &str) -> Vec<Header> {
    text.lines()
        .filter(|l| is_post_header(l))
        .filter_map(|l| {
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

// ===========================================================================
// R1 — concurrent cross-host posts: all messages arrive on both hosts
// ===========================================================================

#[test]
fn r1_concurrent_cross_host_posts_all_arrive() {
    let fx = build_fixture();
    let n_per_agent: usize = 30; // 30×2 = 60 total posts across both hosts

    // Posting threads — alice on host A, bob on host B, both racing.
    let cfg_a = Arc::new(fx.cfg_a.clone());
    let cfg_b = Arc::new(fx.cfg_b.clone());
    let home_a = Arc::new(fx.home_a.clone());
    let home_b = Arc::new(fx.home_b.clone());

    let h_alice = {
        let cfg = Arc::clone(&cfg_a);
        let home = Arc::clone(&home_a);
        thread::spawn(move || {
            for i in 0..n_per_agent {
                giga_post(&home, &cfg, "alice", &format!("alice-{i:02}"), "x").unwrap();
                thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let h_bob = {
        let cfg = Arc::clone(&cfg_b);
        let home = Arc::clone(&home_b);
        thread::spawn(move || {
            for i in 0..n_per_agent {
                giga_post(&home, &cfg, "bob", &format!("bob-{i:02}"), "x").unwrap();
                thread::sleep(Duration::from_millis(20));
            }
        })
    };

    // Sync + merger pump — runs concurrently with the post threads.
    // Ticks faster than the posters so the pipeline has work to do
    // every iteration.
    let stop = Arc::new(AtomicBool::new(false));
    let pump = {
        let stop = Arc::clone(&stop);
        let inbox_a = fx.inbox_a.clone();
        let inbox_b = fx.inbox_b.clone();
        let cfg_a = Arc::clone(&cfg_a);
        let cfg_b = Arc::clone(&cfg_b);
        let home_a = Arc::clone(&home_a);
        let home_b = Arc::clone(&home_b);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                fake_sync_tick(&inbox_a, &inbox_b);
                let _ = giga_merger_once(&home_a, &cfg_a);
                let _ = giga_merger_once(&home_b, &cfg_b);
                thread::sleep(Duration::from_millis(100));
            }
        })
    };

    h_alice.join().unwrap();
    h_bob.join().unwrap();
    // Stop the pump FIRST so the drain loop has the cursor files +
    // slice file copies all to itself (two mergers concurrently on the
    // same HOME race on cursor writes — under-delivery if one's reading
    // a stale cursor value while another's writing).
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();

    // Drain the pipeline. 5 ticks with explicit sync + merger on each
    // side; enough for the final round-trip even on slow CI.
    for _ in 0..5 {
        fake_sync_tick(&fx.inbox_a, &fx.inbox_b);
        giga_merger_once(&home_a, &cfg_a).unwrap();
        giga_merger_once(&home_b, &cfg_b).unwrap();
        thread::sleep(Duration::from_millis(150));
    }

    // Diagnostic dump before assertions so we can see what's where.
    for (label, inbox) in [("host-a", &fx.inbox_a), ("host-b", &fx.inbox_b)] {
        eprintln!("--- {label} inbox contents ---");
        for entry in fs::read_dir(inbox).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            eprintln!("  {name} ({size} bytes)");
        }
    }

    // Assert: each host's merged file contains every post from BOTH agents.
    for (label, inbox) in [("host-a", &fx.inbox_a), ("host-b", &fx.inbox_b)] {
        let merged = inbox.join("alice-bob.md");
        assert!(merged.exists(), "{label}: merged file not created");
        let text = fs::read_to_string(&merged).unwrap();
        let headers = parse_headers(&text);
        let from_alice = headers.iter().filter(|h| h.sender == "alice").count();
        let from_bob = headers.iter().filter(|h| h.sender == "bob").count();
        assert_eq!(
            from_alice, n_per_agent,
            "{label}: expected {n_per_agent} alice posts, got {from_alice}",
        );
        assert_eq!(
            from_bob, n_per_agent,
            "{label}: expected {n_per_agent} bob posts, got {from_bob}",
        );
        // Per-author ordering: within each author's subsequence, subjects
        // are monotonic — own slice is single-writer, so even concurrent
        // sync+merge can't reorder them.
        let alice_subjects: Vec<&str> = headers
            .iter()
            .filter(|h| h.sender == "alice")
            .map(|h| h.subject.as_str())
            .collect();
        for (i, s) in alice_subjects.iter().enumerate() {
            assert_eq!(
                *s,
                format!("alice-{i:02}"),
                "{label}: alice-subject {i} out of order: {s}",
            );
        }
        let bob_subjects: Vec<&str> = headers
            .iter()
            .filter(|h| h.sender == "bob")
            .map(|h| h.subject.as_str())
            .collect();
        for (i, s) in bob_subjects.iter().enumerate() {
            assert_eq!(
                *s,
                format!("bob-{i:02}"),
                "{label}: bob-subject {i} out of order: {s}",
            );
        }
    }
}

// ===========================================================================
// R2 — slice file readable + valid mid-append (rsync read-invariant proxy)
//
// Real rsync over SSH guarantees atomic reads via its own protocol; we
// can't exercise that here without ssh. What we CAN exercise is the
// POSIX invariant our code relies on: while a slice is being appended
// to, any reader (rsync, merger, watcher) sees a complete prefix — not
// a half-written block. If `giga post` ever started splitting a block
// into multiple write_all calls, this test would catch it.
// ===========================================================================

#[test]
fn r2_slice_file_is_always_a_complete_prefix_while_appended() {
    let fx = build_fixture();
    let slice_path = fx.inbox_a.join("alice-bob.wsl-a.md");

    // Writer: post 50 messages over ~3 seconds.
    let cfg_a = fx.cfg_a.clone();
    let home_a = fx.home_a.clone();
    let writer = thread::spawn(move || {
        for i in 0..50 {
            giga_post(&home_a, &cfg_a, "alice", &format!("r2-{i:02}"), "payload").unwrap();
            thread::sleep(Duration::from_millis(40));
        }
    });

    // Reader: continuously read the slice file. Every read must be a
    // valid prefix of every subsequent read — i.e. the file only grows,
    // never reorders bytes mid-write. We record all snapshots and
    // verify the prefix property at the end.
    let reader_slice = slice_path.clone();
    let reader = thread::spawn(move || {
        let mut snapshots: Vec<Vec<u8>> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(bytes) = fs::read(&reader_slice) {
                snapshots.push(bytes);
            }
            thread::sleep(Duration::from_millis(15));
        }
        snapshots
    });

    writer.join().unwrap();
    let snapshots = reader.join().unwrap();

    // The slice file might not exist for the very first few reads —
    // skip empty ones.
    let snapshots: Vec<Vec<u8>> = snapshots.into_iter().filter(|s| !s.is_empty()).collect();
    assert!(
        snapshots.len() >= 5,
        "reader didn't collect enough snapshots ({}); the test may be flaky on slow CI",
        snapshots.len(),
    );

    // Sort by length and verify each shorter snapshot is a prefix of a
    // longer one. (Multiple snapshots at the same length should all be
    // bytewise identical.)
    let mut sorted = snapshots.clone();
    sorted.sort_by_key(|s| s.len());
    for window in sorted.windows(2) {
        let (shorter, longer) = (&window[0], &window[1]);
        assert!(
            longer.starts_with(shorter),
            "snapshot at len {} is NOT a prefix of snapshot at len {} — a writer split a block mid-write OR truncated:\n\
             shorter tail: {:?}\n\
             longer at len(shorter): {:?}",
            shorter.len(),
            longer.len(),
            String::from_utf8_lossy(&shorter[shorter.len().saturating_sub(40)..]),
            String::from_utf8_lossy(&longer[shorter.len().saturating_sub(40)..(shorter.len() + 40).min(longer.len())]),
        );
    }

    // Final sanity: the final file parses to exactly 50 headers.
    let final_text = fs::read_to_string(&slice_path).unwrap();
    let headers = parse_headers(&final_text);
    assert_eq!(
        headers.len(),
        50,
        "final slice should have 50 posts, got {}",
        headers.len()
    );
}

// ===========================================================================
// R3 — merger never double-delivers while a slice grows under it
//
// merger reads slice deltas in a fixed-size buffer derived from
// metadata().len() — if the file grows between the metadata call and
// the read_exact, the merger still reads exactly `len-as-seen-at-metadata`
// bytes; the rest is delivered next tick. No double-delivery should
// ever occur even when sync is racing with merger.
// ===========================================================================

#[test]
fn r3_merger_no_double_delivery_under_racing_slice_growth() {
    let fx = build_fixture();
    // Writer: post 80 messages over ~4 seconds.
    let cfg_a = fx.cfg_a.clone();
    let home_a = fx.home_a.clone();
    let writer = thread::spawn(move || {
        for i in 0..80 {
            giga_post(&home_a, &cfg_a, "alice", &format!("r3-{i:03}"), "x").unwrap();
            thread::sleep(Duration::from_millis(40));
        }
    });

    // Merger pump: run `giga merger --once` repeatedly while the writer
    // is appending. Each run sees a different slice size; cursor must
    // not lose or duplicate any bytes.
    let stop = Arc::new(AtomicBool::new(false));
    let merger_cfg = fx.cfg_a.clone();
    let merger_home = fx.home_a.clone();
    let merger_stop = Arc::clone(&stop);
    let merger = thread::spawn(move || {
        let mut iterations = 0;
        while !merger_stop.load(Ordering::Relaxed) {
            // Best-effort: don't fail the test on a transient merger
            // error (e.g. file not yet created). The CORRECTNESS check
            // is on the final merged file after writer completes.
            let _ = giga_merger_once(&merger_home, &merger_cfg);
            iterations += 1;
            thread::sleep(Duration::from_millis(60));
        }
        iterations
    });

    writer.join().unwrap();
    // Drain: one more merger run after writer finishes to catch the tail.
    thread::sleep(Duration::from_millis(200));
    giga_merger_once(&fx.home_a, &fx.cfg_a).unwrap();
    stop.store(true, Ordering::Relaxed);
    let merger_iterations = merger.join().unwrap();
    assert!(
        merger_iterations >= 10,
        "merger pump didn't run enough iterations ({merger_iterations}); test may be flaky",
    );

    // Assert: merged file has EXACTLY 80 unique r3-NNN subjects, no
    // duplicates. (alice-bob.md is the merged file.)
    let merged = fx.inbox_a.join("alice-bob.md");
    assert!(merged.exists(), "merged file not created");
    let text = fs::read_to_string(&merged).unwrap();
    let headers = parse_headers(&text);
    let r3_subjects: Vec<&str> = headers
        .iter()
        .filter(|h| h.sender == "alice")
        .filter(|h| h.subject.starts_with("r3-"))
        .map(|h| h.subject.as_str())
        .collect();
    assert_eq!(
        r3_subjects.len(),
        80,
        "expected 80 r3 posts in merged, got {}",
        r3_subjects.len(),
    );
    // No duplicates: every subject appears exactly once.
    let mut sorted = r3_subjects.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        r3_subjects.len(),
        "merger double-delivered some posts (dup count = {})",
        r3_subjects.len() - sorted.len(),
    );
    // And every expected subject is present.
    for i in 0..80 {
        let expected = format!("r3-{i:03}");
        assert!(
            r3_subjects.iter().any(|&s| s == expected),
            "missing expected subject {expected}",
        );
    }
}
