//! Fleet configuration: which machines exist, what they should share, and
//! what they must be left alone about (TASK-KOI158, FEAT-KOI039 AC-1).
//!
//! This is the schema and the reader only. Comparing declared state against
//! actual state, and raising proposals from the difference, is TASK-KOI159 —
//! kept apart deliberately so the thing that decides what "equivalent" means
//! is reviewable on its own.
//!
//! # Divergence is a first-class rule, not an exception list
//!
//! The dangerous failure for a fleet tool is helpfully overwriting a setting
//! someone chose on purpose: a different editor on the laptop, a smaller swap
//! on the machine with less disk. [`FleetConfig::is_divergent`] exists so that
//! the comparison layer must ask before proposing, rather than proposing and
//! relying on a human to catch it in review.
//!
//! # TOML, not YAML
//!
//! This card's context said YAML. Every koi runtime config is TOML —
//! `filing.toml`, `cost.toml`, `backup.toml`, `subscriptions.toml` — and there
//! is no YAML crate in the tree. Adding one to a public repo for a single file,
//! against the grain of every other setting koi reads, buys nothing.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One machine in the fleet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    /// Matched against the live hostname, case-insensitively.
    pub hostname: String,
    /// "linux", "macos", "windows".
    pub os: String,
    /// Optional human label for reports.
    #[serde(default)]
    pub label: Option<String>,
}

/// A set of machines expected to share one kind of state.
///
/// `kind` is free text on purpose: this schema should not have to change when
/// TASK-KOI159 adds a fourth or fifth check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EquivalenceClass {
    pub name: String,
    /// What should match — "shell-config", "packages", "dotfiles".
    pub kind: String,
    /// Hostnames in this class.
    pub machines: Vec<String>,
}

/// A per-machine setting koi must never propose to change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DivergenceRule {
    pub hostname: String,
    /// Dotted key of the setting that is deliberately different.
    pub key: String,
    /// Why, so a future reader does not "fix" it.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetConfig {
    pub machines: Vec<Machine>,
    pub equivalence: Vec<EquivalenceClass>,
    pub divergence: Vec<DivergenceRule>,
}

impl FleetConfig {
    pub fn default_path() -> crate::Result<std::path::PathBuf> {
        Ok(crate::state::home_dir()?.join(".config/koi/fleet.toml"))
    }

    /// A missing file is the expected default state and is silent — most
    /// machines are not in a declared fleet. A malformed one warns and falls
    /// back, exactly as `FilingConfig` does, because a typo in a config must
    /// never take the daemon down.
    pub fn load_from(path: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(error = %e, path = %path.display(),
                "malformed fleet.toml, falling back to an empty fleet");
            Self::default()
        })
    }

    pub fn load() -> Self {
        Self::default_path().map_or_else(|_| Self::default(), |p| Self::load_from(&p))
    }

    /// The machine matching `hostname`, case-insensitively — hostnames are not
    /// case-sensitive in practice and a config that says "Studio" should match
    /// a host reporting "studio".
    pub fn machine(&self, hostname: &str) -> Option<&Machine> {
        self.machines
            .iter()
            .find(|m| m.hostname.eq_ignore_ascii_case(hostname))
    }

    /// Equivalence classes this machine belongs to.
    pub fn classes_for(&self, hostname: &str) -> Vec<&EquivalenceClass> {
        self.equivalence
            .iter()
            .filter(|c| c.machines.iter().any(|m| m.eq_ignore_ascii_case(hostname)))
            .collect()
    }

    /// Whether `key` is declared divergent on this machine.
    ///
    /// TASK-KOI159 must consult this before proposing anything: a declared
    /// preference is a decision already taken, and re-proposing against it is
    /// how a fleet tool becomes something you turn off.
    pub fn is_divergent(&self, hostname: &str, key: &str) -> bool {
        self.divergence
            .iter()
            .any(|d| d.hostname.eq_ignore_ascii_case(hostname) && d.key == key)
    }

    /// Divergence rules for a machine, keyed by setting.
    pub fn divergences_for(&self, hostname: &str) -> BTreeMap<&str, Option<&str>> {
        self.divergence
            .iter()
            .filter(|d| d.hostname.eq_ignore_ascii_case(hostname))
            .map(|d| (d.key.as_str(), d.reason.as_deref()))
            .collect()
    }

    /// Machines sharing at least one equivalence class with this one, which is
    /// the set TASK-KOI159 will compare against.
    pub fn peers_of(&self, hostname: &str) -> Vec<&str> {
        let mut peers: Vec<&str> = self
            .classes_for(hostname)
            .iter()
            .flat_map(|c| c.machines.iter())
            .map(String::as_str)
            .filter(|m| !m.eq_ignore_ascii_case(hostname))
            .collect();
        peers.sort_unstable();
        peers.dedup();
        peers
    }
}

/// This machine's hostname, lowercased.
pub fn current_hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_lowercase())
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| {
                    o.status
                        .success()
                        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_lowercase())
                })
                .filter(|h| !h.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // No real hostnames: this repo is public.
    const SAMPLE: &str = r#"
[[machines]]
hostname = "workstation"
os = "linux"
label = "Primary Linux desktop"

[[machines]]
hostname = "laptop"
os = "macos"

[[machines]]
hostname = "windows-box"
os = "windows"

[[equivalence]]
name = "shell"
kind = "shell-config"
machines = ["workstation", "laptop"]

[[equivalence]]
name = "cli-tools"
kind = "packages"
machines = ["workstation", "laptop", "windows-box"]

[[divergence]]
hostname = "laptop"
key = "swap.size_gb"
reason = "smaller disk; a matching swap would not fit"
"#;

    fn cfg() -> FleetConfig {
        toml::from_str(SAMPLE).expect("sample fleet config must parse")
    }

    #[test]
    fn machines_parse_with_os_and_optional_label() {
        let c = cfg();
        assert_eq!(c.machines.len(), 3);
        let w = c.machine("workstation").unwrap();
        assert_eq!(w.os, "linux");
        assert_eq!(w.label.as_deref(), Some("Primary Linux desktop"));
        assert!(c.machine("laptop").unwrap().label.is_none());
    }

    #[test]
    fn hostname_matching_ignores_case() {
        // A config saying "Laptop" must match a host reporting "laptop".
        assert!(cfg().machine("LAPTOP").is_some());
    }

    #[test]
    fn an_unknown_host_is_simply_absent_rather_than_an_error() {
        let c = cfg();
        assert!(c.machine("someone-elses-machine").is_none());
        assert!(c.classes_for("someone-elses-machine").is_empty());
        assert!(c.peers_of("someone-elses-machine").is_empty());
    }

    #[test]
    fn classes_and_peers_resolve_for_a_member() {
        let c = cfg();
        let names: Vec<&str> = c
            .classes_for("workstation")
            .iter()
            .map(|k| k.name.as_str())
            .collect();
        assert_eq!(names, vec!["shell", "cli-tools"]);
        // Peers are deduplicated across classes and exclude self.
        assert_eq!(c.peers_of("workstation"), vec!["laptop", "windows-box"]);
    }

    #[test]
    fn a_declared_divergence_is_visible_to_the_comparison_layer() {
        // The whole point: TASK-KOI159 must be able to ask before proposing.
        let c = cfg();
        assert!(c.is_divergent("laptop", "swap.size_gb"));
        assert!(!c.is_divergent("workstation", "swap.size_gb"));
        assert!(!c.is_divergent("laptop", "shell.prompt"));
    }

    #[test]
    fn divergence_carries_its_reason_so_it_is_not_tidied_away() {
        let c = cfg();
        let d = c.divergences_for("laptop");
        assert_eq!(
            d.get("swap.size_gb").copied().flatten(),
            Some("smaller disk; a matching swap would not fit")
        );
    }

    #[test]
    fn a_missing_config_is_an_empty_fleet_not_a_failure() {
        let c = FleetConfig::load_from(std::path::Path::new("/nonexistent/koi/fleet.toml"));
        assert_eq!(c, FleetConfig::default());
        assert!(c.machines.is_empty());
    }

    #[test]
    fn a_malformed_config_falls_back_rather_than_crashing_the_daemon() {
        let dir = std::env::temp_dir().join(format!("koi-fleet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fleet.toml");
        std::fs::write(&path, "[[machines]]\nthis is not toml {{{").unwrap();
        assert_eq!(FleetConfig::load_from(&path), FleetConfig::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_schema_round_trips() {
        let c = cfg();
        let text = toml::to_string_pretty(&c).unwrap();
        assert_eq!(toml::from_str::<FleetConfig>(&text).unwrap(), c);
    }
}
