//! WebSocket endpoints for live channel tailing.
//!
//! Wire protocol (v0.6.35 Phase D, JSON-encoded text messages):
//!
//!   {"type":"snapshot","posts":[...]}     — sent once on connect,
//!                                            last N posts in
//!                                            chronological order
//!   {"type":"append","post":{...}}        — sent for each post
//!                                            appended after connect
//!   {"type":"error","message":"..."}      — server-side problem;
//!                                            socket may close after
//!
//! The frontend (Phase F) renders snapshot then appends in order.
//!
//! Architecture:
//!   * One `broadcast::Sender<Post>` per `(swarm, channel-file)` lives
//!     in `AppState.tailers`. Subscribers share it.
//!   * A backing tokio task (the "tailer") polls the file every 500ms,
//!     parses on change, and broadcasts the post-tail-diff to all
//!     subscribers.
//!   * Tailer initial state is "current count = posts in file at
//!     spawn"; only NEW posts after that are broadcast. Replay of
//!     historical posts is the WS handler's responsibility (the
//!     snapshot message).

use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, State};
use axum::response::IntoResponse;
use futures_util::sink::SinkExt;
use futures_util::stream::StreamExt;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::broadcast;

use crate::config::Config;
use crate::registry;
use crate::ui::channel as post_parser;
use crate::ui::channel::Post;
use crate::ui::state::AppState;

const SNAPSHOT_POSTS: usize = 50;
const BROADCAST_CAPACITY: usize = 256;
const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum WireEvent<'a> {
    Snapshot { posts: &'a [Post] },
    Append { post: &'a Post },
    Error { message: &'a str },
}

pub async fn ws_channel(
    AxumPath((swarm, file)): AxumPath<(String, String)>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, swarm, file, state))
}

async fn handle_socket(socket: WebSocket, swarm: String, file: String, state: AppState) {
    let (mut sender, mut receiver) = socket.split();

    let path = match resolve_channel_path(&swarm, &file) {
        Ok(p) => p,
        Err(msg) => {
            let _ = send_event(&mut sender, &WireEvent::Error { message: &msg }).await;
            let _ = sender.close().await;
            return;
        }
    };

    // Snapshot: last N posts from disk.
    let snapshot_posts = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let parsed = post_parser::parse(&text);
            let start = parsed.len().saturating_sub(SNAPSHOT_POSTS);
            parsed[start..].to_vec()
        }
        Err(_) => Vec::new(),
    };
    if send_event(
        &mut sender,
        &WireEvent::Snapshot {
            posts: &snapshot_posts,
        },
    )
    .await
    .is_err()
    {
        return;
    }

    // Subscribe (or start) the tailer for this channel.
    let mut rx = ensure_tailer(&state, &swarm, &file, &path).await;

    // Spawn a reader task that drains incoming messages — without
    // it, axum's keepalive pings would accumulate unsent and the
    // socket would eventually back-pressure us. We don't process
    // client messages in v1; just need to keep the read side moving.
    // It also detects client-initiated close; when receiver exits,
    // we signal the writer to stop.
    let reader = tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            match msg {
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    });

    // Forward broadcast events to the socket until either the
    // socket dies or the broadcast is closed.
    loop {
        tokio::select! {
            _ = &mut Box::pin(reader_done(&reader)) => break,
            recv = rx.recv() => match recv {
                Ok(post) => {
                    if send_event(&mut sender, &WireEvent::Append { post: &post }).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = send_event(
                        &mut sender,
                        &WireEvent::Error { message: "subscriber lagged — reconnect" },
                    ).await;
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    // Best-effort close + reader cleanup.
    let _ = sender.close().await;
    reader.abort();
}

async fn reader_done(handle: &tokio::task::JoinHandle<()>) {
    // Await completion by polling lightly — we can't move the JoinHandle
    // into select! repeatedly. This loop checks every 250ms whether the
    // reader exited (client closed or socket errored).
    loop {
        if handle.is_finished() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn send_event<S>(sender: &mut S, event: &WireEvent<'_>) -> Result<(), axum::Error>
where
    S: SinkExt<Message, Error = axum::Error> + Unpin,
{
    let payload = serde_json::to_string(event).map_err(axum::Error::new)?;
    sender.send(Message::Text(Utf8Bytes::from(payload))).await
}

fn resolve_channel_path(swarm: &str, file: &str) -> Result<PathBuf, String> {
    let reg = registry::load().map_err(|e| format!("registry load: {e:#}"))?;
    let entry = reg
        .entries
        .iter()
        .find(|e| e.name == swarm)
        .ok_or_else(|| format!("swarm not found: {swarm}"))?;
    let cfg = Config::load(&entry.config).map_err(|e| format!("config load: {e:#}"))?;
    let channel_meta = cfg
        .channels
        .iter()
        .find(|c| c.file == file)
        .ok_or_else(|| format!("channel not in swarm config: {file}"))?;
    let inbox = match channel_meta.side.as_str() {
        "windows" => cfg.paths.windows_inbox.as_ref(),
        _ => cfg.paths.wsl_inbox.as_ref(),
    }
    .ok_or_else(|| "no inbox path resolved for this channel".to_string())?;
    Ok(inbox.join(file))
}

/// Lookup-or-create the broadcast sender for this `(swarm, file)`,
/// spawning the file-polling tailer task on first creation. Returns
/// a fresh subscriber handle.
async fn ensure_tailer(
    state: &AppState,
    swarm: &str,
    file: &str,
    path: &Path,
) -> broadcast::Receiver<Post> {
    let key = (swarm.to_string(), file.to_string());
    {
        let read = state.tailers.read().await;
        if let Some(tx) = read.get(&key) {
            return tx.subscribe();
        }
    }
    // Promote to write; double-check (another subscriber may have raced us).
    let mut write = state.tailers.write().await;
    if let Some(tx) = write.get(&key) {
        return tx.subscribe();
    }
    let (tx, rx) = broadcast::channel(BROADCAST_CAPACITY);
    write.insert(key.clone(), tx.clone());
    drop(write);
    let path_for_task = path.to_path_buf();
    tokio::spawn(async move {
        run_tailer(path_for_task, tx).await;
    });
    rx
}

/// Polls the file every `POLL_INTERVAL` and broadcasts any newly
/// appended posts to subscribers.
///
/// "Newly appended" = posts beyond the count we saw at spawn time
/// (so historical content isn't replayed on every reconnect — the WS
/// handler explicitly sends a snapshot of the last N for that).
///
/// Lifecycle: runs forever. When there are no subscribers, the
/// broadcast send is a cheap no-op. v2 cleanup: drop+respawn if idle
/// for >5 min.
async fn run_tailer(path: PathBuf, tx: broadcast::Sender<Post>) {
    let mut last_count: usize = match std::fs::read_to_string(&path) {
        Ok(t) => post_parser::parse(&t).len(),
        Err(_) => 0,
    };
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    loop {
        ticker.tick().await;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let posts = post_parser::parse(&text);
        if posts.len() > last_count {
            for post in &posts[last_count..] {
                // Send fails when there are zero receivers — fine,
                // we'll resume broadcasting as soon as someone
                // subscribes. The internal ring still tracks state.
                let _ = tx.send(post.clone());
            }
            last_count = posts.len();
        } else if posts.len() < last_count {
            // File was rewritten / truncated. Reset baseline; don't
            // re-broadcast everything.
            last_count = posts.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn run_tailer_broadcasts_new_posts_after_append() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("ch.md");
        let initial = sample_post("design", "online", "2026-05-28T14:30:00Z", "first body");
        fs::write(&path, &initial).unwrap();

        let (tx, mut rx) = broadcast::channel::<Post>(16);
        let path_clone = path.clone();
        let handle = tokio::spawn(async move { run_tailer(path_clone, tx).await });

        // Tailer's initial count = 1; subscriber should see nothing
        // until we append a second post.
        let mid_recv = tokio::time::timeout(Duration::from_millis(600), rx.recv()).await;
        assert!(
            mid_recv.is_err(),
            "did not expect any post pre-append: {mid_recv:?}"
        );

        let appended = format!(
            "{initial}\n\n{}",
            sample_post("giga", "next", "2026-06-07T22:00:00Z", "second body")
        );
        fs::write(&path, appended).unwrap();

        let post = tokio::time::timeout(Duration::from_millis(1500), rx.recv())
            .await
            .expect("tailer should broadcast within poll budget")
            .expect("broadcast recv should succeed");
        assert_eq!(post.sender, "giga");
        assert_eq!(post.timestamp_iso, "2026-06-07T22:00:00Z");

        handle.abort();
    }

    #[tokio::test]
    async fn ensure_tailer_dedups_per_channel_key() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a.md");
        fs::write(&path, "").unwrap();
        let state = AppState {
            tailers: Arc::new(RwLock::new(Default::default())),
        };

        let _rx1 = ensure_tailer(&state, "s1", "a.md", &path).await;
        let _rx2 = ensure_tailer(&state, "s1", "a.md", &path).await;
        let _rx3 = ensure_tailer(&state, "s2", "a.md", &path).await;

        let map = state.tailers.read().await;
        assert_eq!(map.len(), 2, "should have one tailer per (swarm, file) key");
        assert!(map.contains_key(&("s1".into(), "a.md".into())));
        assert!(map.contains_key(&("s2".into(), "a.md".into())));
    }

    #[tokio::test]
    async fn run_tailer_resets_after_file_truncated() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("ch.md");
        fs::write(
            &path,
            format!(
                "{}{}{}",
                sample_post("a", "x", "2026-01-01T00:00:00Z", "x"),
                "\n\n",
                sample_post("b", "y", "2026-01-02T00:00:00Z", "y"),
            ),
        )
        .unwrap();

        let (tx, mut rx) = broadcast::channel::<Post>(16);
        let path_clone = path.clone();
        let handle = tokio::spawn(async move { run_tailer(path_clone, tx).await });

        // Let initial state settle.
        tokio::time::sleep(Duration::from_millis(700)).await;
        // Drain anything spurious.
        while rx.try_recv().is_ok() {}

        // Truncate to single post.
        fs::write(&path, sample_post("c", "z", "2026-01-03T00:00:00Z", "z")).unwrap();
        // No append yet → no broadcast.
        let early = tokio::time::timeout(Duration::from_millis(800), rx.recv()).await;
        assert!(
            early.is_err(),
            "truncation alone should not broadcast: {early:?}"
        );

        // Now append a NEW post past the truncated baseline.
        let again = format!(
            "{}\n\n{}",
            sample_post("c", "z", "2026-01-03T00:00:00Z", "z"),
            sample_post("d", "w", "2026-01-04T00:00:00Z", "w"),
        );
        fs::write(&path, again).unwrap();
        let post = tokio::time::timeout(Duration::from_millis(1500), rx.recv())
            .await
            .expect("expected new post after truncate-then-append")
            .expect("broadcast recv should succeed");
        assert_eq!(post.sender, "d");

        handle.abort();
    }

    fn sample_post(sender: &str, subject: &str, ts: &str, body: &str) -> String {
        format!("===\n[{sender}] {subject} — {ts}\n===\n\n{body}\n\n===")
    }
}
