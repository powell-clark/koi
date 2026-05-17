//! Managed-zone protocol — other systems drop `.koi-managed-by` markers to
//! claim directories that koi must not touch. See ADR-0014.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub const MARKER_FILENAME: &str = ".koi-managed-by";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ManagedScope {
    /// Marker applies to the directory containing the marker AND everything beneath.
    Recursive,
    /// Marker applies only to direct children — subdirectories can have their own markers.
    DirectChildren,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedZone {
    pub system: String,
    pub owner: Option<String>,
    pub scope: ManagedScope,
    pub contact: Option<String>,
    /// Path that contained the marker.
    #[serde(skip)]
    pub root: PathBuf,
}

/// Load a `.koi-managed-by` marker from a directory, if one exists and is valid.
pub fn load_zone(dir: &Path) -> Option<ManagedZone> {
    let marker = dir.join(MARKER_FILENAME);
    let text = std::fs::read_to_string(&marker).ok()?;
    // Minimal TOML parser: don't want a full toml dep just for this.
    // Format is stable and simple — refuse anything we don't recognise.
    let mut system = None;
    let mut owner = None;
    let mut scope = None;
    let mut contact = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim().trim_matches('"').to_string();
        match k {
            "system" => system = Some(v),
            "owner" => owner = Some(v),
            "scope" => {
                scope = Some(match v.as_str() {
                    "recursive" => ManagedScope::Recursive,
                    "direct-children" => ManagedScope::DirectChildren,
                    _ => return None,
                })
            }
            "contact" => contact = Some(v),
            _ => {}
        }
    }
    Some(ManagedZone {
        system: system?,
        owner,
        scope: scope.unwrap_or(ManagedScope::Recursive),
        contact,
        root: dir.to_path_buf(),
    })
}

/// Cache of resolved managed zones, queried during a single scan.
#[derive(Debug, Default)]
pub struct ZoneCache {
    /// Directory root -> zone metadata.
    zones: HashMap<PathBuf, ManagedZone>,
}

impl ZoneCache {
    /// Call this when walking into a new directory to eagerly register any markers.
    pub fn register_dir(&mut self, dir: &Path) {
        if let Some(zone) = load_zone(dir) {
            self.zones.insert(dir.to_path_buf(), zone);
        }
    }

    /// True if `path` is inside a managed zone.
    pub fn is_managed(&self, path: &Path) -> bool {
        for (root, zone) in &self.zones {
            if !path.starts_with(root) {
                continue;
            }
            match zone.scope {
                ManagedScope::Recursive => return true,
                ManagedScope::DirectChildren => {
                    if let Some(parent) = path.parent() {
                        if parent == root {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    pub fn zones(&self) -> impl Iterator<Item = &ManagedZone> {
        self.zones.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("koi-test-{}", rand_suffix()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn rand_suffix() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        format!("{nanos:x}")
    }

    #[test]
    fn loads_valid_marker() {
        let dir = tmpdir();
        let mut f = std::fs::File::create(dir.join(MARKER_FILENAME)).unwrap();
        writeln!(
            f,
            "system = \"the-book\"\nowner = \"em@pc.com\"\nscope = \"recursive\""
        )
        .unwrap();
        let zone = load_zone(&dir).unwrap();
        assert_eq!(zone.system, "the-book");
        assert_eq!(zone.scope, ManagedScope::Recursive);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_marker_returns_none() {
        let dir = tmpdir();
        assert!(load_zone(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recursive_scope_covers_descendants() {
        let root = PathBuf::from("/home/user/Finance");
        let mut cache = ZoneCache::default();
        cache.zones.insert(
            root.clone(),
            ManagedZone {
                system: "the-book".into(),
                owner: None,
                scope: ManagedScope::Recursive,
                contact: None,
                root: root.clone(),
            },
        );
        assert!(cache.is_managed(&PathBuf::from("/home/user/Finance/2026/invoice.pdf")));
        assert!(!cache.is_managed(&PathBuf::from("/home/user/Documents/x.pdf")));
    }
}
