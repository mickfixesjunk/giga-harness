//! Tailscale roster + identity, parsed from `tailscale status --json`.
//!
//! Two call sites historically diverged: `hosts` parsed the JSON with
//! serde, while `setup_remote_node` string-grepped the same output for
//! `DNSName`. This is the one parser. Invocation tries native `tailscale`
//! first and falls back to the Windows-side install so a WSL distro that
//! inherits its host's tailnet still works.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

/// One node in the tailnet roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetNode {
    /// FQDN with the trailing dot stripped, e.g. `host-a.tail0000.ts.net`.
    pub dns_name: String,
    /// Short name, e.g. `host-a`.
    pub host_name: String,
    /// OS hint: `linux` | `windows` | `macOS` | …
    pub os: String,
}

/// Parsed `tailscale status --json`: this node plus every peer, and the
/// backend state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetStatus {
    pub self_node: Option<TailnetNode>,
    pub peers: Vec<TailnetNode>,
    /// `BackendState`, e.g. `Running`, `NeedsLogin`, `Stopped`.
    pub backend_state: Option<String>,
}

impl TailnetStatus {
    /// The flat node list: this node followed by every peer.
    pub fn nodes(&self) -> Vec<TailnetNode> {
        let mut v = Vec::new();
        if let Some(n) = &self.self_node {
            v.push(n.clone());
        }
        v.extend(self.peers.iter().cloned());
        v
    }

    /// This node's FQDN.
    pub fn self_dns_name(&self) -> Option<String> {
        self.self_node.as_ref().map(|n| n.dns_name.clone())
    }

    /// Whether the tailnet backend is up and authenticated.
    pub fn is_running(&self) -> bool {
        self.backend_state.as_deref() == Some("Running")
    }
}

/// Parse `tailscale status --json` bytes. Pure — testable without the
/// subprocess.
pub fn parse_status(bytes: &[u8]) -> Result<TailnetStatus> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).context("parsing tailscale status --json output")?;
    let self_node = v.get("Self").and_then(extract_node);
    let peers = v
        .get("Peer")
        .and_then(|p| p.as_object())
        .map(|m| m.values().filter_map(extract_node).collect())
        .unwrap_or_default();
    let backend_state = v
        .get("BackendState")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    Ok(TailnetStatus {
        self_node,
        peers,
        backend_state,
    })
}

fn extract_node(v: &serde_json::Value) -> Option<TailnetNode> {
    let dns_name = v
        .get("DNSName")
        .and_then(|s| s.as_str())?
        .trim_end_matches('.')
        .to_string();
    let host_name = v
        .get("HostName")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let os = v
        .get("OS")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    Some(TailnetNode {
        dns_name,
        host_name,
        os,
    })
}

/// Invoke `tailscale status --json`, trying native `tailscale` on PATH
/// then common Windows install paths (for WSL distros inheriting the
/// Windows host's Tailscale).
pub fn invoke_status_json() -> Result<Vec<u8>> {
    if let Ok(out) = Command::new("tailscale")
        .args(["status", "--json"])
        .output()
    {
        if out.status.success() {
            return Ok(out.stdout);
        }
    }
    for path in [
        "/mnt/c/Program Files/Tailscale/tailscale.exe",
        "/mnt/c/Program Files (x86)/Tailscale/tailscale.exe",
    ] {
        if Path::new(path).exists() {
            let out = Command::new(path)
                .args(["status", "--json"])
                .output()
                .with_context(|| format!("invoking {path} status --json"))?;
            if out.status.success() {
                return Ok(out.stdout);
            }
        }
    }
    Err(anyhow!(
        "tailscale CLI not found on PATH and no Windows install detected \
         at /mnt/c/Program Files/Tailscale/. Install Tailscale or run this \
         from a WSL distro on a host where Windows-side Tailscale is set up."
    ))
}

/// Full parsed status from a live `tailscale status --json` call.
pub fn status() -> Result<TailnetStatus> {
    parse_status(&invoke_status_json()?)
}

/// The flat tailnet roster (this node + peers) from a live call.
pub fn roster() -> Result<Vec<TailnetNode>> {
    Ok(status()?.nodes())
}

/// Whether Tailscale is logged into a tailnet. Mirrors the historic
/// `tailscale status` exit-code check (success ⇒ logged in).
pub fn is_logged_in() -> bool {
    Command::new("tailscale")
        .arg("status")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = br#"
{
  "BackendState": "Running",
  "Self": {
    "DNSName": "neo.tail0000.ts.net.",
    "HostName": "neo",
    "OS": "windows"
  },
  "Peer": {
    "abc123": { "DNSName": "host-a.tail0000.ts.net.", "HostName": "host-a", "OS": "linux" },
    "def456": { "DNSName": "trinity.tail0000.ts.net.", "HostName": "trinity", "OS": "windows" }
  }
}
"#;

    #[test]
    fn parses_self_and_peers() {
        let st = parse_status(SAMPLE).unwrap();
        assert_eq!(st.self_dns_name().as_deref(), Some("neo.tail0000.ts.net"));
        assert_eq!(st.peers.len(), 2);
        let nodes = st.nodes();
        assert_eq!(nodes.len(), 3);
        assert!(nodes
            .iter()
            .any(|n| n.host_name == "host-a" && n.os == "linux"));
    }

    #[test]
    fn strips_trailing_dot_from_dns_name() {
        let st = parse_status(SAMPLE).unwrap();
        assert!(!st.self_dns_name().unwrap().ends_with('.'));
    }

    #[test]
    fn reads_backend_state() {
        assert!(parse_status(SAMPLE).unwrap().is_running());
        let needs =
            br#"{"BackendState":"NeedsLogin","Self":{"DNSName":"x.","HostName":"x","OS":"linux"}}"#;
        assert!(!parse_status(needs).unwrap().is_running());
    }

    #[test]
    fn errors_on_invalid_json() {
        assert!(parse_status(b"not json").is_err());
    }
}
