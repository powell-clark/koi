//! Shared directory-size measurement — actual disk usage, not apparent size.
//!
//! Extracted after TASK-KOI174 found `~/.docker` measuring 64GiB (a sparse
//! file's logical length) instead of the real 8.1GB (allocated disk blocks),
//! and TASK-KOI188 found the identical duplicated logic in CacheMonitor.
//! Single shared implementation now — a third duplicate should not recur.

use std::path::Path;
use walkdir::WalkDir;

/// Sums actual allocated disk blocks under `path` (matching `du`'s default
/// behaviour), deduplicated by `(device, inode)` so a hardlinked file's
/// blocks are counted once, not once per link. On non-Unix targets (no
/// inode/block concept), falls back to logical file length.
#[cfg(unix)]
pub fn dir_size(path: &Path) -> u64 {
    use std::collections::HashSet;
    use std::os::unix::fs::MetadataExt;

    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .filter(|m| seen_inodes.insert((m.dev(), m.ino())))
        .map(|m| m.blocks() * 512)
        .sum()
}

#[cfg(not(unix))]
pub fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    #[cfg(unix)]
    fn dir_size_counts_disk_blocks_not_apparent_length_for_sparse_files() {
        let dir =
            std::env::temp_dir().join(format!("koi-fs-size-sparse-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sparse_path = dir.join("sparse.bin");
        let f = std::fs::File::create(&sparse_path).unwrap();
        f.set_len(1024 * 1024 * 1024).unwrap();
        drop(f);

        let measured = dir_size(&dir);
        assert!(
            measured < 512 * 1024 * 1024,
            "expected sparse file to measure near-zero disk usage, got {measured} bytes"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn dir_size_counts_hardlinked_file_once() {
        let dir =
            std::env::temp_dir().join(format!("koi-fs-size-hardlink-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("original.bin");
        let mut f = std::fs::File::create(&original).unwrap();
        f.write_all(&[0u8; 4096]).unwrap();
        drop(f);
        let link = dir.join("hardlink.bin");
        std::fs::hard_link(&original, &link).unwrap();

        let measured = dir_size(&dir);

        let solo =
            std::env::temp_dir().join(format!("koi-fs-size-solo-test-{}", std::process::id()));
        std::fs::create_dir_all(&solo).unwrap();
        let mut sf = std::fs::File::create(solo.join("f.bin")).unwrap();
        sf.write_all(&[0u8; 4096]).unwrap();
        drop(sf);
        let single_file_size = dir_size(&solo);

        assert_eq!(measured, single_file_size, "hardlinked file counted twice");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&solo);
    }
}
