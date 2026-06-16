//! Read-side resolvers: map agents to hosts/runtimes and channels to
//! their on-disk paths. No mutation, no I/O beyond the host-fs path
//! translation in `channel_path`.

use std::path::PathBuf;

use anyhow::{anyhow, Result};

use super::schema::{Agent, Channel, Config};

impl Config {
    /// The effective host for an agent: explicit `agent.host` if set,
    /// otherwise `this_host` (the local machine). Returns `None` only
    /// when neither is known — local-only mode pre-remote-channels.
    pub fn agent_host<'a>(&'a self, agent: &'a Agent) -> Option<&'a str> {
        agent.host.as_deref().or(self.this_host.as_deref())
    }

    /// v0.6.0: resolve which runtime an agent uses. Priority:
    /// explicit `agent.runtime` → `project.runtime` → `Runtime::Claude`
    /// (backward compat default).
    pub fn agent_runtime(&self, agent: &Agent) -> crate::runtime::Runtime {
        agent.runtime.or(self.project.runtime).unwrap_or_default()
    }

    /// True when every participant of the channel lives on `this_host`
    /// (or `[[hosts]]` is empty — today's local-only mode). When true,
    /// `post` can take the fast-path direct write to `<channel>.md`
    /// instead of writing to a per-host slice file.
    ///
    /// Unknown participants are treated as remote (conservative) — this
    /// should never happen for a validated config but defensive nonetheless.
    pub fn channel_is_local(&self, ch: &Channel) -> bool {
        if self.hosts.is_empty() {
            return true; // pre-remote-channels world: nothing is "remote"
        }
        let Some(this) = self.this_host.as_deref() else {
            return true; // degenerate but validated config wouldn't reach here
        };
        ch.participants.iter().all(|p| {
            self.agents
                .iter()
                .find(|a| a.name == *p)
                .and_then(|a| self.agent_host(a))
                .map(|h| h == this)
                .unwrap_or(false)
        })
    }

    /// Resolve a channel file to its absolute path on this host,
    /// using the configured inbox dirs. The configured dir may be in
    /// the other side's path form (e.g., windows_inbox = "/mnt/c/..."
    /// for WSL convenience); `to_host_fs` translates it to whatever
    /// the current host's filesystem expects.
    ///
    /// Uses the per-host [paths] override under `[[hosts]]` when this_host
    /// is known and the override is set (v0.3.2+ asymmetric-path
    /// support); falls back to the global `[paths]` otherwise.
    pub fn channel_path(&self, ch: &Channel) -> Result<PathBuf> {
        let dir = self
            .inbox_for_host_side(self.this_host.as_deref(), &ch.side)
            .ok_or_else(|| anyhow!("no inbox path for channel `{}` (side={})", ch.file, ch.side))?;
        Ok(crate::fs_paths::to_host_fs(&dir).join(&ch.file))
    }

    /// Returns the inbox dir to use for a given host + side. Priority:
    ///   1. `[[hosts]].paths.<side>_inbox` (per-host explicit override; v0.3.2+)
    ///   2. `[[hosts]].remote_inbox_dir` (v0.2 back-compat — applies to
    ///      the wsl side only since that's what it was designed for)
    ///   3. global `[paths].<side>_inbox` (homogeneous-path fallback)
    ///
    /// `host_name = None` means "no host context" (legacy local-only
    /// swarm or pre-host-resolution); always uses the global path.
    pub fn inbox_for_host_side(&self, host_name: Option<&str>, side: &str) -> Option<PathBuf> {
        if let Some(name) = host_name {
            if let Some(host) = self.hosts.iter().find(|h| h.name == name) {
                if let Some(host_paths) = &host.paths {
                    let p = match side {
                        "wsl" => host_paths.wsl_inbox.as_ref(),
                        "windows" => host_paths.windows_inbox.as_ref(),
                        _ => None,
                    };
                    if let Some(p) = p {
                        return Some(p.clone());
                    }
                }
                // v0.2 back-compat shim — remote_inbox_dir is a single
                // path with no side distinction; treat it as wsl-side.
                if side == "wsl" {
                    if let Some(p) = &host.remote_inbox_dir {
                        return Some(p.clone());
                    }
                }
            }
        }
        match side {
            "wsl" => self.paths.wsl_inbox.clone(),
            "windows" => self.paths.windows_inbox.clone(),
            _ => None,
        }
    }

    pub fn agent_by_name(&self, name: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::path::PathBuf;

    fn minimal() -> &'static str {
        r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"

[[agents]]
name = "b"
workdir = "/h/b"
role = "."
platform = "wsl"

[[channels]]
file = "a-b.md"
side = "wsl"
participants = ["a", "b"]
"#
    }

    fn minimal_two_host() -> String {
        r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0000.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0000.ts.net"

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
"#
        .to_string()
    }

    #[test]
    fn channel_path_resolves_wsl_side() {
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        let ch = &cfg.channels[0];
        let p = cfg.channel_path(ch).unwrap();
        assert!(p.ends_with("a-b.md"));
        assert!(p.starts_with("/tmp/i"));
    }

    #[test]
    fn parses_hosts_table() {
        // load_str_for_test goes through validate(), which would reject
        // a config with [[hosts]] non-empty and this_host=None. Bypass
        // by going direct to toml::from_str + manual validate.
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        assert_eq!(cfg.hosts.len(), 2);
        assert_eq!(cfg.hosts[0].name, "wsl-a");
        assert_eq!(cfg.hosts[1].tailnet_hostname, "wsl-b.tail0000.ts.net");
    }

    #[test]
    fn agent_host_resolves_to_hosts_entry() {
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        let alice = cfg.agent_by_name("alice").unwrap();
        let bob = cfg.agent_by_name("bob").unwrap();
        assert_eq!(cfg.agent_host(alice), Some("wsl-a"));
        assert_eq!(cfg.agent_host(bob), Some("wsl-b"));
    }

    #[test]
    fn channel_is_local_when_all_participants_share_this_host() {
        let body = minimal_two_host().replace(
            r#"participants = ["alice", "bob"]"#,
            r#"participants = ["alice"]"#,
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        let ch = &cfg.channels[0];
        assert!(
            cfg.channel_is_local(ch),
            "alice is on wsl-a (this_host) -> local fast-path"
        );
    }

    #[test]
    fn channel_is_not_local_when_participants_span_hosts() {
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        let ch = &cfg.channels[0];
        assert!(
            !cfg.channel_is_local(ch),
            "alice@wsl-a + bob@wsl-b spans hosts -> slice path"
        );
    }

    #[test]
    fn channel_is_local_when_this_host_is_the_other_side() {
        // Same config viewed from wsl-b: alice is remote, bob is local.
        // The channel is still cross-host (not local fast-path).
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("wsl-b".into());
        cfg.validate().unwrap();
        let ch = &cfg.channels[0];
        assert!(!cfg.channel_is_local(ch));
    }

    #[test]
    fn inbox_for_host_side_uses_per_host_paths_override() {
        // v0.3.8: agents need explicit host= in multi-host swarms.
        let body = format!(
            r#"{}
[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "wsl-b.tail0.ts.net"
paths.wsl_inbox = "/home/alice/projects/inbox"
"#,
            minimal()
                .replace(
                    "platform = \"wsl\"\n\n[[agents]]",
                    "platform = \"wsl\"\nhost = \"wsl-a\"\n\n[[agents]]",
                )
                .replace(
                    "platform = \"wsl\"\n\n[[channels]]",
                    "platform = \"wsl\"\nhost = \"wsl-a\"\n\n[[channels]]",
                )
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        // wsl-a has no override → falls through to global /tmp/i
        assert_eq!(
            cfg.inbox_for_host_side(Some("wsl-a"), "wsl"),
            Some(PathBuf::from("/tmp/i"))
        );
        // wsl-b has per-host override → uses that
        assert_eq!(
            cfg.inbox_for_host_side(Some("wsl-b"), "wsl"),
            Some(PathBuf::from("/home/alice/projects/inbox"))
        );
    }

    #[test]
    fn inbox_for_host_side_falls_back_to_remote_inbox_dir_v02_compat() {
        // v0.2 swarms used [[hosts]].remote_inbox_dir before the
        // explicit per-host [paths] field existed. Keep working.
        // v0.3.8: agents need explicit host=.
        let body = format!(
            r#"{}
[[hosts]]
name = "wsl-a"
tailnet_hostname = "a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "b.tail0.ts.net"
remote_inbox_dir = "/legacy/path"
"#,
            minimal()
                .replace(
                    "platform = \"wsl\"\n\n[[agents]]",
                    "platform = \"wsl\"\nhost = \"wsl-a\"\n\n[[agents]]",
                )
                .replace(
                    "platform = \"wsl\"\n\n[[channels]]",
                    "platform = \"wsl\"\nhost = \"wsl-a\"\n\n[[channels]]",
                )
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        assert_eq!(
            cfg.inbox_for_host_side(Some("wsl-b"), "wsl"),
            Some(PathBuf::from("/legacy/path"))
        );
    }

    #[test]
    fn inbox_for_host_side_unknown_host_uses_global() {
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        assert_eq!(
            cfg.inbox_for_host_side(Some("ghost"), "wsl"),
            Some(PathBuf::from("/tmp/i"))
        );
    }

    #[test]
    fn inbox_for_host_side_no_host_context_uses_global() {
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        assert_eq!(
            cfg.inbox_for_host_side(None, "wsl"),
            Some(PathBuf::from("/tmp/i"))
        );
    }

    #[test]
    fn channel_path_uses_per_host_override_when_this_host_set() {
        // The killer test: a peer with asymmetric paths must NOT try to
        // resolve channels against the operator's local-only inbox.
        // v0.3.8: add explicit host= to the agents from minimal().
        let body = format!(
            r#"{}
[[hosts]]
name = "wsl-a"
tailnet_hostname = "a.tail0.ts.net"

[[hosts]]
name = "wsl-b"
tailnet_hostname = "b.tail0.ts.net"
paths.wsl_inbox = "/home/alice/projects/inbox"

[[channels]]
file = "alice-bob.md"
side = "wsl"
participants = ["a", "b"]
"#,
            minimal()
                .trim_end_matches(|c: char| c == '\n')
                .replace(
                    "platform = \"wsl\"\n\n[[agents]]",
                    "platform = \"wsl\"\nhost = \"wsl-a\"\n\n[[agents]]",
                )
                .replace(
                    "platform = \"wsl\"\n\n[[channels]]",
                    "platform = \"wsl\"\nhost = \"wsl-b\"\n\n[[channels]]",
                )
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-b".into());
        cfg.validate().unwrap();
        let ch = cfg
            .channels
            .iter()
            .find(|c| c.file == "alice-bob.md")
            .unwrap();
        let path = cfg.channel_path(ch).unwrap();
        assert!(
            path.starts_with("/home/alice/projects/inbox"),
            "channel_path on wsl-b should use wsl-b's override, got {}",
            path.display()
        );
    }
}
