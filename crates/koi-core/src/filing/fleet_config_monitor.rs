//! FleetConfigMonitor — compares this machine's actual state against its
//! declared fleet equivalence classes and surfaces discrepancies for review.
//! TASK-KOI159, FEAT-KOI039 AC-2..AC-5. Design decisions: ADR-0024.
//!
//! Read-only and local-only tonight: this machine publishes its own
//! fingerprints to a state file and reads whatever peer fingerprints
//! already exist locally. Nothing here moves a file, or any data, between
//! machines — that wiring is TASK-KOI258, deliberately deferred (ADR-0024
//! §2) because it touches the real, live-synced `$HOME` git repo and cannot
//! be verified without a second machine.

use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    filing::{FileMonitor, Proposal, ProposedAction, ScanContext},
    fleet::{self, EquivalenceClass, FleetConfig},
    Result,
};

/// One fingerprint plus the real file it was taken from. `path` grounds the
/// [`Proposal`] so `state::supersede_stale_proposals`'s existence check
/// keeps working unmodified rather than needing a synthetic path special
/// case (ADR-0024 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub hash: String,
    pub path: PathBuf,
}

/// This machine's fingerprints, one per equivalence-class kind it can
/// check. Serialised to `~/.local/state/koi/fleet-state.json` today; a peer
/// copy under `fleet-peers/<hostname>.json` is read the same way once
/// TASK-KOI258 wires the actual cross-machine sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FleetSnapshot {
    pub fingerprints: BTreeMap<String, Fingerprint>,
}

pub struct FleetConfigMonitor {
    cfg: FleetConfig,
    hostname: String,
    home: PathBuf,
    state_dir: PathBuf,
}

impl FleetConfigMonitor {
    pub fn new() -> Result<Self> {
        let home = crate::state::home_dir()?;
        let state_dir = home.join(".local/state/koi");
        Ok(Self {
            cfg: FleetConfig::load(),
            hostname: fleet::current_hostname().unwrap_or_default(),
            home,
            state_dir,
        })
    }

    fn own_fingerprints(&self) -> BTreeMap<String, Fingerprint> {
        let mut out = BTreeMap::new();
        if let Some(fp) = shell_config_fingerprint(&self.home) {
            out.insert("shell-config".to_string(), fp);
        }
        if let Some(fp) = packages_fingerprint() {
            out.insert("packages".to_string(), fp);
        }
        if let Some(fp) = dotfiles_fingerprint(&self.home) {
            out.insert("dotfiles".to_string(), fp);
        }
        out
    }

    fn peer_snapshot(&self, peer_hostname: &str) -> Option<FleetSnapshot> {
        let path = self
            .state_dir
            .join("fleet-peers")
            .join(format!("{peer_hostname}.json"));
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn publish_own_snapshot(&self, own: &BTreeMap<String, Fingerprint>) {
        let snapshot = FleetSnapshot {
            fingerprints: own.clone(),
        };
        let Ok(text) = serde_json::to_string_pretty(&snapshot) else {
            return;
        };
        let _ = std::fs::create_dir_all(&self.state_dir);
        let _ = std::fs::write(self.state_dir.join("fleet-state.json"), text);
    }
}

impl FileMonitor for FleetConfigMonitor {
    fn name(&self) -> &'static str {
        "FleetConfigMonitor"
    }

    fn roots(&self) -> Vec<PathBuf> {
        // Not a directory scan — nothing to contribute to the shared
        // managed-zone cache.
        Vec::new()
    }

    fn scan(&self, _ctx: &ScanContext) -> Result<Vec<Proposal>> {
        // An undeclared host has nothing to compare (ADR-0024 §2) — the
        // expected state for this machine tonight, not a fault.
        if self.cfg.machine(&self.hostname).is_none() {
            return Ok(Vec::new());
        }

        let own = self.own_fingerprints();
        self.publish_own_snapshot(&own);

        let classes = self.cfg.classes_for(&self.hostname);
        let peer_snapshots: BTreeMap<String, FleetSnapshot> = self
            .cfg
            .peers_of(&self.hostname)
            .into_iter()
            .filter_map(|p| self.peer_snapshot(p).map(|s| (p.to_string(), s)))
            .collect();

        let mismatches = compare(&own, &peer_snapshots, &classes, |kind| {
            self.cfg.is_divergent(&self.hostname, kind)
        });

        Ok(mismatches
            .into_iter()
            .map(|m| {
                Proposal::new(
                    "FleetConfigMonitor",
                    m.path,
                    ProposedAction::Review {
                        summary: m.summary.clone(),
                    },
                    m.summary,
                    1.0,
                )
            })
            .collect())
    }
}

/// One detected discrepancy.
struct Mismatch {
    path: PathBuf,
    summary: String,
}

/// Pure comparison: given this machine's fingerprints, whatever peer
/// snapshots are available, and the classes this machine belongs to, name
/// every kind whose fingerprint differs from at least one peer sharing that
/// class — unless divergence says this machine is deliberately different
/// for that kind. No I/O; fully testable with synthetic maps (ADR-0024
/// §1/§3, TASK-KOI159 AC-3/AC-4).
fn compare(
    own: &BTreeMap<String, Fingerprint>,
    peers: &BTreeMap<String, FleetSnapshot>,
    classes: &[&EquivalenceClass],
    is_divergent: impl Fn(&str) -> bool,
) -> Vec<Mismatch> {
    let mut out = Vec::new();
    let mut checked_kinds = std::collections::BTreeSet::new();

    for class in classes {
        if !checked_kinds.insert(class.kind.clone()) {
            continue; // One check per kind, even if two classes share it.
        }
        if is_divergent(&class.kind) {
            continue;
        }
        let Some(own_fp) = own.get(&class.kind) else {
            continue; // This machine has nothing to say about this kind.
        };
        for peer_hostname in &class.machines {
            let Some(peer_hash) = peers
                .get(peer_hostname.as_str())
                .and_then(|s| s.fingerprints.get(&class.kind))
                .map(|fp| &fp.hash)
            else {
                continue; // No snapshot from this peer yet, or self.
            };
            if peer_hash != &own_fp.hash {
                out.push(Mismatch {
                    path: own_fp.path.clone(),
                    summary: format!(
                        "{} differs from peer '{peer_hostname}' (equivalence class '{}')",
                        class.kind, class.name
                    ),
                });
                break; // One mismatch per kind is enough to raise for review.
            }
        }
    }
    out
}

fn hash_str(content: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Hash of whichever of `.bashrc`/`.zshrc` exist, concatenated. `path` is
/// the first one found, so the mismatch always points at a real file.
fn shell_config_fingerprint(home: &Path) -> Option<Fingerprint> {
    let candidates = [home.join(".bashrc"), home.join(".zshrc")];
    let mut content = String::new();
    let mut chosen_path = None;
    for c in &candidates {
        if let Ok(text) = std::fs::read_to_string(c) {
            content.push_str(&text);
            chosen_path.get_or_insert_with(|| c.clone());
        }
    }
    Some(Fingerprint {
        hash: hash_str(&content),
        path: chosen_path?,
    })
}

/// Linux only for now — brew/winget equivalents are follow-up work once a
/// second live machine exists (ADR-0024). Grounded on `/var/lib/dpkg/status`
/// so the proposal's existence check has a real, always-present file.
fn packages_fingerprint() -> Option<Fingerprint> {
    let path = PathBuf::from("/var/lib/dpkg/status");
    if !path.exists() {
        return None;
    }
    let output = std::process::Command::new("dpkg-query")
        .args(["-W", "-f=${Package} ${Version}\n"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort_unstable();
    Some(Fingerprint {
        hash: hash_str(&lines.join("\n")),
        path,
    })
}

/// This machine's dotfiles "version" IS the `$HOME` repo's HEAD commit that
/// `koi-dotfiles-sync.timer` last pushed (ADR-0024 §1).
fn dotfiles_fingerprint(home: &Path) -> Option<Fingerprint> {
    let head_path = home.join(".git/HEAD");
    if !head_path.exists() {
        return None;
    }
    let output = std::process::Command::new("git")
        .args(["-C", &home.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if hash.is_empty() {
        return None;
    }
    Some(Fingerprint {
        hash,
        path: head_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(name: &str, kind: &str, machines: &[&str]) -> EquivalenceClass {
        EquivalenceClass {
            name: name.to_string(),
            kind: kind.to_string(),
            machines: machines.iter().map(|m| m.to_string()).collect(),
        }
    }

    fn fp(hash: &str, path: &str) -> Fingerprint {
        Fingerprint {
            hash: hash.to_string(),
            path: PathBuf::from(path),
        }
    }

    fn snapshot(entries: &[(&str, &str, &str)]) -> FleetSnapshot {
        FleetSnapshot {
            fingerprints: entries
                .iter()
                .map(|(kind, hash, path)| (kind.to_string(), fp(hash, path)))
                .collect(),
        }
    }

    #[test]
    fn matching_fingerprint_produces_no_mismatch() {
        let own = BTreeMap::from([("shell-config".to_string(), fp("abc", "/home/me/.bashrc"))]);
        let peers = BTreeMap::from([(
            "laptop".to_string(),
            snapshot(&[("shell-config", "abc", "/home/laptop/.bashrc")]),
        )]);
        let classes = [class("shell", "shell-config", &["workstation", "laptop"])];
        let classes_ref: Vec<&EquivalenceClass> = classes.iter().collect();
        let out = compare(&own, &peers, &classes_ref, |_| false);
        assert!(out.is_empty());
    }

    #[test]
    fn differing_fingerprint_raises_one_mismatch_named_by_kind_and_peer() {
        let own = BTreeMap::from([("shell-config".to_string(), fp("abc", "/home/me/.bashrc"))]);
        let peers = BTreeMap::from([(
            "laptop".to_string(),
            snapshot(&[("shell-config", "different", "/home/laptop/.bashrc")]),
        )]);
        let classes = [class("shell", "shell-config", &["workstation", "laptop"])];
        let classes_ref: Vec<&EquivalenceClass> = classes.iter().collect();
        let out = compare(&own, &peers, &classes_ref, |_| false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, PathBuf::from("/home/me/.bashrc"));
        assert!(out[0].summary.contains("shell-config"));
        assert!(out[0].summary.contains("laptop"));
    }

    #[test]
    fn declared_divergence_suppresses_a_real_mismatch() {
        // STORY-KOI058: koi never proposes to override a declared
        // preference, even when the fingerprints genuinely differ.
        let own = BTreeMap::from([("packages".to_string(), fp("abc", "/var/lib/dpkg/status"))]);
        let peers = BTreeMap::from([(
            "laptop".to_string(),
            snapshot(&[("packages", "different", "/var/lib/dpkg/status")]),
        )]);
        let classes = [class("cli-tools", "packages", &["workstation", "laptop"])];
        let classes_ref: Vec<&EquivalenceClass> = classes.iter().collect();
        let out = compare(&own, &peers, &classes_ref, |kind| kind == "packages");
        assert!(out.is_empty());
    }

    #[test]
    fn no_peer_snapshot_yet_is_silence_not_a_mismatch() {
        let own = BTreeMap::from([("dotfiles".to_string(), fp("abc", "/home/me/.git/HEAD"))]);
        let peers = BTreeMap::new(); // TASK-KOI258 not landed: no peer has ever synced.
        let classes = [class("dotfiles-sync", "dotfiles", &["workstation", "laptop"])];
        let classes_ref: Vec<&EquivalenceClass> = classes.iter().collect();
        let out = compare(&own, &peers, &classes_ref, |_| false);
        assert!(out.is_empty());
    }

    #[test]
    fn a_kind_shared_by_two_classes_is_checked_once() {
        let own = BTreeMap::from([("packages".to_string(), fp("abc", "/var/lib/dpkg/status"))]);
        let peers = BTreeMap::from([(
            "laptop".to_string(),
            snapshot(&[("packages", "different", "/var/lib/dpkg/status")]),
        )]);
        let classes = [
            class("cli-tools", "packages", &["workstation", "laptop"]),
            class("cli-tools-again", "packages", &["workstation", "laptop"]),
        ];
        let classes_ref: Vec<&EquivalenceClass> = classes.iter().collect();
        let out = compare(&own, &peers, &classes_ref, |_| false);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn no_own_fingerprint_for_a_kind_is_silence_not_a_crash() {
        let own = BTreeMap::new(); // e.g. dpkg-query unavailable on this OS.
        let peers = BTreeMap::from([(
            "laptop".to_string(),
            snapshot(&[("packages", "x", "/var/lib/dpkg/status")]),
        )]);
        let classes = [class("cli-tools", "packages", &["workstation", "laptop"])];
        let classes_ref: Vec<&EquivalenceClass> = classes.iter().collect();
        let out = compare(&own, &peers, &classes_ref, |_| false);
        assert!(out.is_empty());
    }

    #[test]
    fn an_undeclared_host_scans_to_zero_proposals() {
        // This host, tonight: no fleet.toml entry for it at all (ADR-0024).
        let monitor = FleetConfigMonitor {
            cfg: FleetConfig::default(),
            hostname: "this-machine-is-not-in-any-fleet".to_string(),
            home: crate::state::home_dir().unwrap(),
            state_dir: std::env::temp_dir().join("koi-fleet-test-unused"),
        };
        let ctx = ScanContext::new_now();
        let out = monitor.scan(&ctx).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn hash_is_stable_and_content_sensitive() {
        assert_eq!(hash_str("same"), hash_str("same"));
        assert_ne!(hash_str("same"), hash_str("different"));
    }
}
