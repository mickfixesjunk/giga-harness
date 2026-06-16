//! Semantic validation of a parsed [`Config`]. `validate()` is the
//! public entry point; the per-invariant helpers below are private and
//! run in a fixed order. Each preserves the exact checks + error
//! messages from the pre-split monolithic validate().

use anyhow::{anyhow, Result};

use super::schema::Config;

impl Config {
    /// Cross-check the config: every channel participant resolves
    /// to an agent, every channel side has its inbox dir defined,
    /// at most one bench scheduler, etc.
    pub fn validate(&self) -> Result<()> {
        self.validate_hosts()?;
        self.validate_channels()?;
        self.validate_schedulers()?;
        self.validate_agents()?;
        self.validate_swarm_bosses()?;
        Ok(())
    }

    /// [[hosts]] uniqueness, agent.host resolution, this_host
    /// resolution, and the degenerate "hosts declared but this_host
    /// unknown" combo.
    fn validate_hosts(&self) -> Result<()> {
        // [[hosts]] uniqueness + agent.host resolution.
        let mut seen_host_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for h in &self.hosts {
            if !seen_host_names.insert(h.name.as_str()) {
                return Err(anyhow!(
                    "duplicate [[hosts]] entry `{}` (host names must be unique)",
                    h.name,
                ));
            }
        }

        for a in &self.agents {
            if let Some(host) = &a.host {
                if !seen_host_names.contains(host.as_str()) {
                    return Err(anyhow!(
                        "agent `{}` has host `{}` which isn't in [[hosts]]",
                        a.name,
                        host,
                    ));
                }
            }
        }

        // this_host (if present) must resolve to a hosts entry.
        if let Some(th) = &self.this_host {
            if !seen_host_names.contains(th.as_str()) {
                return Err(anyhow!(
                    "this_host = `{}` isn't in [[hosts]] (check the sibling this_host.toml)",
                    th,
                ));
            }
        }

        // Degenerate combo: [[hosts]] declared but no this_host known.
        // The swarm has remote topology but this machine doesn't know
        // its own identity — slice writes have no suffix to use.
        if !self.hosts.is_empty() && self.this_host.is_none() {
            return Err(anyhow!(
                "[[hosts]] is declared but this_host is unknown (create a sibling this_host.toml with `this_host = \"<host-name>\"`)",
            ));
        }

        Ok(())
    }

    /// Every channel participant resolves to an agent and every
    /// channel side has its inbox dir defined.
    fn validate_channels(&self) -> Result<()> {
        let agent_names: std::collections::HashSet<&str> =
            self.agents.iter().map(|a| a.name.as_str()).collect();

        for ch in &self.channels {
            for p in &ch.participants {
                if !agent_names.contains(p.as_str()) {
                    return Err(anyhow!(
                        "channel `{}` lists participant `{}` which isn't in [[agents]]",
                        ch.file,
                        p,
                    ));
                }
            }
            match ch.side.as_str() {
                "wsl" => {
                    if self.paths.wsl_inbox.is_none() {
                        return Err(anyhow!(
                            "channel `{}` is side=wsl but paths.wsl_inbox is not set",
                            ch.file,
                        ));
                    }
                }
                "windows" => {
                    if self.paths.windows_inbox.is_none() {
                        return Err(anyhow!(
                            "channel `{}` is side=windows but paths.windows_inbox is not set",
                            ch.file,
                        ));
                    }
                }
                other => {
                    return Err(anyhow!(
                        "channel `{}` has unknown side `{}` (want \"wsl\" or \"windows\")",
                        ch.file,
                        other,
                    ));
                }
            }
        }

        Ok(())
    }

    /// At most one bench scheduler across the swarm.
    fn validate_schedulers(&self) -> Result<()> {
        let schedulers: Vec<&str> = self
            .agents
            .iter()
            .filter(|a| a.bench_scheduler)
            .map(|a| a.name.as_str())
            .collect();
        if schedulers.len() > 1 {
            return Err(anyhow!(
                "multiple agents flagged as bench_scheduler: {:?}. Only one per host.",
                schedulers,
            ));
        }

        Ok(())
    }

    /// Per-agent platform validity and the multi-host explicit-`host`
    /// requirement.
    fn validate_agents(&self) -> Result<()> {
        for a in &self.agents {
            if a.platform != "wsl" && a.platform != "windows" {
                return Err(anyhow!(
                    "agent `{}` has unknown platform `{}` (want \"wsl\" or \"windows\")",
                    a.name,
                    a.platform,
                ));
            }
        }

        // v0.3.8 Bug 4 fix: when [[hosts]] is non-empty, every agent
        // MUST have an explicit `host = "..."` field. Pre-fix:
        // `agent_host()` falls back to this_host for host-less agents,
        // which means the SAME canonical TOML reads differently on each
        // host — agents collapse to "wherever I am running", silently
        // misrouting channels. The user's bootstrap report (Bug 4)
        // showed `giga hosts` reporting all 5 agents as local on each
        // of 2 hosts. Require explicit-host now; remediation is to
        // add `host = "<host-name>"` to each agent block.
        if !self.hosts.is_empty() {
            let host_names: std::collections::HashSet<&str> =
                self.hosts.iter().map(|h| h.name.as_str()).collect();
            let unhosted: Vec<&str> = self
                .agents
                .iter()
                .filter(|a| a.host.is_none())
                .map(|a| a.name.as_str())
                .collect();
            if !unhosted.is_empty() {
                return Err(anyhow!(
                    "agents missing `host = \"...\"` in a multi-host swarm: {:?}. \
                     When [[hosts]] is non-empty, every [[agents]] block must declare \
                     which host it runs on (one of: {:?}). The pre-v0.3.8 fallback to \
                     this_host silently misrouted channels on the peer.",
                    unhosted,
                    host_names.iter().copied().collect::<Vec<_>>(),
                ));
            }
        }

        Ok(())
    }

    /// At most one swarm_boss per host, and swarm_boss agents must be
    /// platform="wsl" (sync + merger are POSIX-only).
    fn validate_swarm_bosses(&self) -> Result<()> {
        // v0.3.6: at most one swarm_boss per host. Bucketed by the
        // resolved host (agent.host or this_host fallback). See
        // SWARM_BOSS_DESIGN.md §3.2.
        let mut bosses_per_host: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for a in &self.agents {
            if !a.swarm_boss {
                continue;
            }
            if a.platform != "wsl" {
                return Err(anyhow!(
                    "agent `{}` is flagged swarm_boss but has platform `{}` — \
                     sync + merger are POSIX-only; swarm_boss must be platform=\"wsl\"",
                    a.name,
                    a.platform,
                ));
            }
            // Resolve host: explicit `agent.host`, else this_host, else
            // "<local>" sentinel (legacy local-only swarm).
            let host_key = a
                .host
                .as_deref()
                .or(self.this_host.as_deref())
                .unwrap_or("<local>");
            bosses_per_host
                .entry(host_key)
                .or_default()
                .push(a.name.as_str());
        }
        for (host, names) in &bosses_per_host {
            if names.len() > 1 {
                return Err(anyhow!(
                    "multiple agents flagged as swarm_boss on host `{}`: {:?}. At most one per host.",
                    host,
                    names,
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

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
    fn rejects_unknown_participant() {
        let body = minimal().replace(
            r#"participants = ["a", "b"]"#,
            r#"participants = ["a", "ghost"]"#,
        );
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err.to_string().contains("ghost"));
        assert!(err.to_string().contains("isn't in [[agents]]"));
    }

    #[test]
    fn rejects_unknown_channel_side() {
        let body = minimal().replace(r#"side = "wsl""#, r#"side = "macos""#);
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err.to_string().contains("unknown side"));
    }

    #[test]
    fn rejects_windows_channel_without_windows_inbox() {
        let body = minimal().replace(r#"side = "wsl""#, r#"side = "windows""#);
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err.to_string().contains("windows_inbox"));
    }

    #[test]
    fn rejects_wsl_channel_without_wsl_inbox() {
        let body = r#"
[project]
name = "t"

[paths]
windows_inbox = "/tmp/iw"

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
"#;
        let err = Config::load_str_for_test(body).unwrap_err();
        assert!(err.to_string().contains("wsl_inbox"));
    }

    #[test]
    fn rejects_multiple_bench_schedulers() {
        let body = minimal()
            .replace(
                r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl""#,
                r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
bench_scheduler = true"#,
            )
            .replace(
                r#"name = "b"
workdir = "/h/b"
role = "."
platform = "wsl""#,
                r#"name = "b"
workdir = "/h/b"
role = "."
platform = "wsl"
bench_scheduler = true"#,
            );
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err.to_string().contains("multiple agents"));
    }

    #[test]
    fn accepts_single_bench_scheduler() {
        let body = minimal().replace(
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl""#,
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
bench_scheduler = true"#,
        );
        let cfg = Config::load_str_for_test(&body).unwrap();
        assert_eq!(cfg.agents.iter().filter(|a| a.bench_scheduler).count(), 1);
    }

    /// v0.3.6 S1 (SWARM_BOSS_DESIGN.md): two agents both flagged
    /// swarm_boss on the same host is a validation error. Mirrors
    /// the bench_scheduler-uniqueness rule.
    #[test]
    fn rejects_multiple_swarm_bosses_on_same_host() {
        let body = minimal()
            .replace(
                r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl""#,
                r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
swarm_boss = true"#,
            )
            .replace(
                r#"name = "b"
workdir = "/h/b"
role = "."
platform = "wsl""#,
                r#"name = "b"
workdir = "/h/b"
role = "."
platform = "wsl"
swarm_boss = true"#,
            );
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err
            .to_string()
            .contains("multiple agents flagged as swarm_boss"));
    }

    /// v0.3.6 S2: one boss per host across a multi-host swarm is OK.
    #[test]
    fn accepts_one_swarm_boss_per_host_on_multi_host_swarm() {
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[hosts]]
name = "host-a"
tailnet_hostname = "host-a.tail0.ts.net"

[[hosts]]
name = "host-b"
tailnet_hostname = "host-b.tail0.ts.net"

[[agents]]
name = "boss-a"
workdir = "/h/boss-a"
role = "."
platform = "wsl"
host = "host-a"
swarm_boss = true

[[agents]]
name = "boss-b"
workdir = "/h/boss-b"
role = "."
platform = "wsl"
host = "host-b"
swarm_boss = true
"#;
        // Need a this_host or validation rejects [[hosts]] without it.
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("giga-harness.toml");
        std::fs::write(&cfg_path, body).unwrap();
        std::fs::write(
            tmp.path().join("this_host.toml"),
            "this_host = \"host-a\"\n",
        )
        .unwrap();
        let cfg = Config::load(&cfg_path).unwrap();
        assert_eq!(cfg.agents.iter().filter(|a| a.swarm_boss).count(), 2);
    }

    #[test]
    fn rejects_swarm_boss_on_windows_platform() {
        let body = minimal().replace(
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl""#,
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "windows"
swarm_boss = true"#,
        );
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(
            err.to_string().contains("swarm_boss") && err.to_string().contains("POSIX-only"),
            "got: {err}",
        );
    }

    #[test]
    fn rejects_unknown_platform() {
        let body = minimal().replace(
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "wsl""#,
            r#"name = "a"
workdir = "/h/a"
role = "."
platform = "linux""#,
        );
        let err = Config::load_str_for_test(&body).unwrap_err();
        assert!(err.to_string().contains("unknown platform"));
    }

    #[test]
    fn config_with_no_channels_validates() {
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"

[[agents]]
name = "a"
workdir = "/h/a"
role = "."
platform = "wsl"
"#;
        let cfg = Config::load_str_for_test(body).unwrap();
        assert_eq!(cfg.agents.len(), 1);
        assert_eq!(cfg.channels.len(), 0);
    }

    #[test]
    fn config_with_no_agents_validates() {
        let body = r#"
[project]
name = "t"

[paths]
wsl_inbox = "/tmp/i"
"#;
        let cfg = Config::load_str_for_test(body).unwrap();
        assert_eq!(cfg.agents.len(), 0);
    }

    #[test]
    fn unknown_agent_host_fails_validation() {
        let body = minimal_two_host().replace(r#"host = "wsl-b""#, r#"host = "ghost-host""#);
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("ghost-host"));
        assert!(err.to_string().contains("isn't in [[hosts]]"));
    }

    #[test]
    fn duplicate_host_names_fail_validation() {
        let body = minimal_two_host().replace(
            r#"name = "wsl-b"
tailnet_hostname = "wsl-b.tail0000.ts.net""#,
            r#"name = "wsl-a"
tailnet_hostname = "wsl-a-dup.tail0000.ts.net""#,
        );
        let cfg: Config = toml::from_str(&body).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate"));
        assert!(err.to_string().contains("wsl-a"));
    }

    #[test]
    fn this_host_must_resolve_to_hosts_entry() {
        let mut cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        cfg.this_host = Some("nowhere".into());
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("this_host"));
        assert!(err.to_string().contains("nowhere"));
    }

    #[test]
    fn hosts_declared_but_this_host_missing_is_degenerate() {
        let cfg: Config = toml::from_str(&minimal_two_host()).unwrap();
        // this_host left as None
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("this_host"));
        assert!(err.to_string().contains("this_host.toml"));
    }

    #[test]
    fn empty_hosts_section_works_with_no_this_host() {
        // Backward compat: today's local-only configs have no [[hosts]]
        // and no this_host. Must validate cleanly.
        let cfg = Config::load_str_for_test(minimal()).unwrap();
        assert!(cfg.hosts.is_empty());
        assert!(cfg.this_host.is_none());
        // And channel_is_local returns true (local fast-path)
        let ch = &cfg.channels[0];
        assert!(cfg.channel_is_local(ch));
    }

    /// v0.3.8 Bug 4 fix (inverted from the v0.2 fallback test): when
    /// [[hosts]] is non-empty, agents MUST have an explicit `host =`
    /// field. The old fallback to this_host silently misrouted
    /// channels because the SAME canonical TOML resolved differently
    /// on each host. Validation now rejects the missing-host case.
    #[test]
    fn agent_without_host_field_in_multi_host_swarm_is_rejected() {
        let body = minimal_two_host().replace(r#"host = "wsl-a""#, "");
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        let err = cfg.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("missing `host") && msg.contains("alice"),
            "expected multi-host explicit-host error; got: {msg}"
        );
    }

    #[test]
    fn host_with_optional_ssh_user() {
        let body = minimal_two_host().replace(
            r#"[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0000.ts.net""#,
            r#"[[hosts]]
name = "wsl-a"
tailnet_hostname = "wsl-a.tail0000.ts.net"
ssh_user = "alice""#,
        );
        let mut cfg: Config = toml::from_str(&body).unwrap();
        cfg.this_host = Some("wsl-a".into());
        cfg.validate().unwrap();
        assert_eq!(cfg.hosts[0].ssh_user.as_deref(), Some("alice"));
        assert_eq!(cfg.hosts[1].ssh_user, None); // omitted defaults to None
    }
}
