//! Cross-root duplicate-file detection. See ADR-0021 for the one-way-door
//! decisions this module implements: dedicated group tables rather than a
//! `Proposal` variant, blake3 content hash, keep-oldest-by-mtime, and dedupe
//! never deletes — it only ever finds and reports groups (this module) or
//! stages a reversible move to trash (TASK-KOI210). `scan` never mutates
//! anything on disk.
//!
//! Deliberately not a `FileMonitor`: duplicate groups span roots, and this
//! walk is genuinely recursive — unlike the root-level-only FileMonitor
//! contract that keeps the six health monitors inside their 200ms budget.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::filing::managed_zone::ZoneCache;

/// Matches the ADR-0021 default — large media/VM-image files are common and
/// rarely worth a full hash pass in the default configuration.
pub const DEFAULT_MAX_SIZE_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateMember {
    pub path: PathBuf,
    pub mtime: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateGroup {
    /// Hex-encoded blake3 hash of the shared content — also the natural,
    /// collision-free `group_id` for persistence (state.rs MIGRATION_V4).
    pub content_hash: String,
    pub size: u64,
    /// Oldest member by mtime.
    pub keeper: PathBuf,
    pub members: Vec<DuplicateMember>,
}

/// Recursively scan `roots` for duplicate-content files.
///
/// Size-bucketed first: only files whose size collides with another file's
/// are hashed at all. Hardlinked files are deduplicated by `(dev, ino)`
/// before hashing, so two links to the same inode are never reported as
/// duplicates of each other. Managed zones (`.koi-managed-by`) are honoured
/// at every depth — the zone cache is populated on the fly as the walk
/// descends, not via the shallow root-plus-direct-children
/// `ZoneCache::discover` the FileMonitor scan path uses.
pub fn scan(roots: &[PathBuf], max_size_bytes: u64) -> Vec<DuplicateGroup> {
    let mut zones = ZoneCache::default();
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();

    for root in roots {
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if entry.file_type().is_dir() {
                zones.register_dir(path);
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            if zones.is_managed(path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let size = meta.len();
            if size == 0 || size > max_size_bytes {
                continue;
            }
            by_size.entry(size).or_default().push(path.to_path_buf());
        }
    }

    let candidates: Vec<PathBuf> = by_size
        .into_values()
        .filter(|group| group.len() > 1)
        .flatten()
        .collect();

    let candidates = dedup_hardlinks(candidates);

    let hashed: Vec<(PathBuf, String, u64, DateTime<Utc>)> = candidates
        .par_iter()
        .filter_map(|path| {
            let hash = hash_file(path).ok()?;
            let meta = std::fs::metadata(path).ok()?;
            let mtime: DateTime<Utc> = meta.modified().ok()?.into();
            Some((path.clone(), hash, meta.len(), mtime))
        })
        .collect();

    let mut by_hash: HashMap<String, Vec<(PathBuf, u64, DateTime<Utc>)>> = HashMap::new();
    for (path, hash, size, mtime) in hashed {
        by_hash.entry(hash).or_default().push((path, size, mtime));
    }

    by_hash
        .into_iter()
        .filter(|(_, members)| members.len() > 1)
        .map(|(content_hash, mut members)| {
            members.sort_by_key(|(_, _, mtime)| *mtime);
            let keeper = members[0].0.clone();
            let size = members[0].1;
            DuplicateGroup {
                content_hash,
                size,
                keeper,
                members: members
                    .into_iter()
                    .map(|(path, _, mtime)| DuplicateMember { path, mtime })
                    .collect(),
            }
        })
        .collect()
}

/// Reclaimable bytes across every group — every member except the keeper.
pub fn reclaimable_bytes(groups: &[DuplicateGroup]) -> u64 {
    groups
        .iter()
        .map(|g| g.size * (g.members.len() as u64 - 1))
        .sum()
}

#[cfg(unix)]
fn dedup_hardlinks(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    use std::{collections::HashSet, os::unix::fs::MetadataExt};
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    paths
        .into_iter()
        .filter(|p| match std::fs::metadata(p) {
            Ok(m) => seen.insert((m.dev(), m.ino())),
            Err(_) => false,
        })
        .collect()
}

#[cfg(not(unix))]
fn dedup_hardlinks(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    fn tmpdir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("koi-dedupe-{prefix}-{nanos:x}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_with_mtime(path: &Path, contents: &[u8], age_secs_ago: u64) {
        fs::write(path, contents).unwrap();
        let mtime = SystemTime::now() - Duration::from_secs(age_secs_ago);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    #[test]
    fn finds_a_planted_duplicate_group_and_keeps_the_oldest_by_mtime() {
        let root = tmpdir("basic");
        write_with_mtime(&root.join("oldest.txt"), b"same content", 300);
        write_with_mtime(&root.join("middle.txt"), b"same content", 200);
        write_with_mtime(&root.join("newest.txt"), b"same content", 100);
        write_with_mtime(&root.join("unrelated.txt"), b"different content!", 100);

        let groups = scan(std::slice::from_ref(&root), DEFAULT_MAX_SIZE_BYTES);

        assert_eq!(groups.len(), 1, "exactly one duplicate group expected");
        let g = &groups[0];
        assert_eq!(g.members.len(), 3);
        assert_eq!(g.size, "same content".len() as u64);
        assert_eq!(g.keeper, root.join("oldest.txt"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cross_root_duplicates_are_grouped_together() {
        let root_a = tmpdir("cross-a");
        let root_b = tmpdir("cross-b");
        write_with_mtime(&root_a.join("a.txt"), b"shared across roots", 50);
        write_with_mtime(&root_b.join("b.txt"), b"shared across roots", 10);

        let groups = scan(&[root_a.clone(), root_b.clone()], DEFAULT_MAX_SIZE_BYTES);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members.len(), 2);
        assert_eq!(groups[0].keeper, root_a.join("a.txt"));

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_pair_is_not_reported_as_a_duplicate() {
        let root = tmpdir("hardlink");
        let original = root.join("original.bin");
        fs::write(&original, b"hardlinked content").unwrap();
        fs::hard_link(&original, root.join("linked.bin")).unwrap();

        let groups = scan(std::slice::from_ref(&root), DEFAULT_MAX_SIZE_BYTES);

        assert_eq!(
            groups.len(),
            0,
            "a hardlinked pair is one file, not a duplicate pair"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skips_files_over_the_size_limit() {
        let root = tmpdir("oversize");
        write_with_mtime(&root.join("a.bin"), &[7u8; 1000], 20);
        write_with_mtime(&root.join("b.bin"), &[7u8; 1000], 10);

        let groups = scan(std::slice::from_ref(&root), 100); // limit well under 1000 bytes

        assert_eq!(groups.len(), 0);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skips_zero_byte_files() {
        let root = tmpdir("empty");
        write_with_mtime(&root.join("a.empty"), b"", 20);
        write_with_mtime(&root.join("b.empty"), b"", 10);

        let groups = scan(std::slice::from_ref(&root), DEFAULT_MAX_SIZE_BYTES);

        assert_eq!(groups.len(), 0, "empty files are not meaningful duplicates");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn honours_a_managed_zone_at_arbitrary_depth() {
        // Marker two levels below root — proves the walk registers zones as
        // it descends rather than only checking root + direct children
        // (which is all the shallow ZoneCache::discover covers).
        let root = tmpdir("managed-deep");
        fs::create_dir_all(root.join("A/B/C")).unwrap();
        fs::write(
            root.join("A/B/.koi-managed-by"),
            "system = \"other\"\nscope = \"recursive\"",
        )
        .unwrap();
        write_with_mtime(&root.join("A/B/C/managed1.txt"), b"managed dupe", 50);
        write_with_mtime(&root.join("A/B/C/managed2.txt"), b"managed dupe", 40);
        write_with_mtime(&root.join("unmanaged1.txt"), b"free dupe", 30);
        write_with_mtime(&root.join("unmanaged2.txt"), b"free dupe", 20);

        let groups = scan(std::slice::from_ref(&root), DEFAULT_MAX_SIZE_BYTES);

        assert_eq!(
            groups.len(),
            1,
            "only the unmanaged pair should surface as a group"
        );
        assert_eq!(groups[0].members.len(), 2);
        assert!(
            groups[0].keeper.ends_with("unmanaged2.txt")
                || groups[0].keeper.ends_with("unmanaged1.txt")
        );
        assert!(groups[0]
            .members
            .iter()
            .all(|m| !m.path.starts_with(root.join("A/B"))));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reclaimable_bytes_counts_every_member_except_one_keeper_per_group() {
        let group = DuplicateGroup {
            content_hash: "abc".into(),
            size: 100,
            keeper: PathBuf::from("/a"),
            members: vec![
                DuplicateMember {
                    path: PathBuf::from("/a"),
                    mtime: Utc::now(),
                },
                DuplicateMember {
                    path: PathBuf::from("/b"),
                    mtime: Utc::now(),
                },
                DuplicateMember {
                    path: PathBuf::from("/c"),
                    mtime: Utc::now(),
                },
            ],
        };
        assert_eq!(reclaimable_bytes(&[group]), 200);
    }
}
