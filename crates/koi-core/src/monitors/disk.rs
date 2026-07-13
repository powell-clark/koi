//! DiskMonitor — directory growth tracking with 6-hour size cache.
//!
//! Watches seven accumulation directories under `$HOME`, caches sizes, flags
//! directories over the warn
//! threshold, and detects stale cache directories by access time.

use chrono::Utc;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use walkdir::WalkDir;

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

const WATCH_DIRS: &[&str] = &[
    ".cache", ".config", ".docker", ".local", ".pyenv", ".nvm", ".npm",
];

// Linux/XDG layout — mirrors cache.rs KNOWN_CACHES paths.
#[cfg(not(target_os = "macos"))]
const STALE_CACHE_DIRS: &[&str] = &[
    ".cache/ms-playwright",
    ".cache/pre-commit",
    ".cache/typescript",
    ".cache/puppeteer",
    ".npm",
];

// macOS layout — same tools under ~/Library/Caches; npm keeps ~/.npm.
#[cfg(target_os = "macos")]
const STALE_CACHE_DIRS: &[&str] = &[
    "Library/Caches/ms-playwright",
    "Library/Caches/pre-commit",
    "Library/Caches/typescript",
    "Library/Caches/puppeteer",
    ".npm",
];

const CACHE_TTL_SECS: u64 = 6 * 3600;
const STALE_DAYS: u64 = 30;
const WARN_BYTES: u64 = 10 * 1024 * 1024 * 1024; // 10 GiB

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    timestamp: u64,
    sizes: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheState {
    Fresh,
    Stale,
    FirstRun,
}

pub struct DiskMonitor {
    home: PathBuf,
    cache_path: PathBuf,
}

impl DiskMonitor {
    pub fn new() -> Result<Self> {
        let home = dirs_home()?;
        let cache_path = home.join(".cache/koi/disk-cache.json");
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self { home, cache_path })
    }

    fn sizes(&self) -> (Vec<(String, u64)>, CacheState) {
        if let Some(cache) = self.read_cache() {
            let age = unix_now().saturating_sub(cache.timestamp);
            if age < CACHE_TTL_SECS {
                return (cache.sizes, CacheState::Fresh);
            }
            // Cache expired — recompute and persist, same as a first run.
            let sizes = self.compute_sizes();
            let _ = self.write_cache(&sizes);
            return (sizes, CacheState::Stale);
        }
        let sizes = self.compute_sizes();
        let _ = self.write_cache(&sizes);
        (sizes, CacheState::FirstRun)
    }

    fn compute_sizes(&self) -> Vec<(String, u64)> {
        let targets: Vec<(String, PathBuf)> = WATCH_DIRS
            .iter()
            .filter_map(|name| {
                let path = self.home.join(name);
                path.exists().then(|| (name.to_string(), path))
            })
            .collect();

        let mut sizes: Vec<(String, u64)> = targets
            .par_iter()
            .map(|(name, path)| (name.clone(), dir_size(path)))
            .collect();

        sizes.sort_by_key(|s| std::cmp::Reverse(s.1));
        sizes
    }

    fn read_cache(&self) -> Option<CacheFile> {
        let bytes = fs::read(&self.cache_path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn write_cache(&self, sizes: &[(String, u64)]) -> Result<()> {
        let payload = CacheFile {
            timestamp: unix_now(),
            sizes: sizes.to_vec(),
        };
        let tmp = self.cache_path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&payload)?)?;
        fs::rename(tmp, &self.cache_path)?;
        Ok(())
    }

    fn stale_caches(&self) -> Vec<String> {
        let cutoff = STALE_DAYS * 86_400;
        STALE_CACHE_DIRS
            .iter()
            .filter_map(|rel| {
                let path = self.home.join(rel);
                let meta = fs::metadata(&path).ok()?;
                let atime = meta
                    .accessed()
                    .ok()?
                    .duration_since(UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                let age = unix_now().saturating_sub(atime);
                (age > cutoff).then(|| format!("{rel} (not accessed in {} days)", age / 86_400))
            })
            .collect()
    }
}

impl Monitor for DiskMonitor {
    fn name(&self) -> &'static str {
        "DiskMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let (sizes, cache_state) = self.sizes();
        let stale = self.stale_caches();

        let over_threshold: Vec<_> = sizes.iter().filter(|(_, s)| *s > WARN_BYTES).collect();
        let status = match over_threshold.len() {
            0 => HealthStatus::Healthy,
            1..=3 => HealthStatus::Warning,
            _ => HealthStatus::Critical,
        };

        let observations = sizes
            .iter()
            .map(|(name, size)| Observation {
                key: name.clone(),
                value: serde_json::json!({
                    "bytes": size,
                    "gib": (*size as f64) / (1024f64.powi(3)),
                }),
                severity: if *size > WARN_BYTES {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            })
            .collect();

        let mut suggestions: Vec<Suggestion> = over_threshold
            .iter()
            .take(3)
            .map(|(name, size)| Suggestion {
                message: format!(
                    "{name} is {:.1} GiB (exceeds 10 GiB threshold)",
                    (*size as f64) / (1024f64.powi(3))
                ),
                severity: Severity::Warning,
                action_hint: Some(format!("Inspect {name} for reclaimable caches")),
            })
            .collect();

        if !stale.is_empty() {
            suggestions.push(Suggestion {
                message: format!("Clean {} stale caches to reclaim space", stale.len()),
                severity: Severity::Info,
                action_hint: Some(stale.join("; ")),
            });
        }

        let _ = cache_state; // retained for future history/telemetry use
        Ok(MonitorReport {
            monitor: self.name().to_string(),
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
            collected_at: Utc::now(),
            observations,
            suggestions,
        })
    }
}

// Sums actual allocated disk blocks (matching `du`'s default behaviour),
// deduplicated by (device, inode) to avoid double-counting hardlinks.
//
// TASK-KOI174 root cause: a naive `metadata().len()` sum reports *apparent*
// (logical) size, which is wrong for sparse files — confirmed on this exact
// machine, ~/.docker contains a large sparse file whose apparent size is
// ~64GiB but whose real disk consumption (`st_blocks`) is ~8.1GB, matching
// `du -sh` exactly (`du --apparent-size` independently confirmed ~65G,
// i.e. matched the OLD buggy behaviour bit for bit). Hardlink dedup is kept
// as a correctness belt-and-braces for directories that do use them heavily
// (Docker's overlay2 driver is a common case, just not present in this
// particular ~/.docker) — st_blocks alone doesn't protect against a
// hardlinked file's blocks being counted once per link. On non-Unix targets
// (no inode/block concept), falls back to the logical-length sum.
#[cfg(unix)]
fn dir_size(path: &Path) -> u64 {
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
fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn dirs_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| crate::error::Error::Config("$HOME not set".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    #[cfg(unix)]
    fn dir_size_counts_disk_blocks_not_apparent_length_for_sparse_files() {
        // TASK-KOI174 regression: a naive metadata().len() sum reports a
        // sparse file's logical size, not its real disk consumption. A file
        // truncated to 1GB with nothing written should occupy ~0 bytes on
        // disk, not 1GB.
        let dir = std::env::temp_dir().join(format!("koi-disk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sparse_path = dir.join("sparse.bin");
        let f = std::fs::File::create(&sparse_path).unwrap();
        f.set_len(1024 * 1024 * 1024).unwrap(); // 1 GiB logical, 0 bytes written
        drop(f);

        let measured = dir_size(&dir);
        // Real disk usage should be far below the 1GiB apparent size —
        // filesystem block accounting varies, so assert well under half.
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
            std::env::temp_dir().join(format!("koi-disk-hardlink-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("original.bin");
        let mut f = std::fs::File::create(&original).unwrap();
        f.write_all(&[0u8; 4096]).unwrap();
        drop(f);
        let link = dir.join("hardlink.bin");
        std::fs::hard_link(&original, &link).unwrap();

        let measured = dir_size(&dir);
        let single_file_size = dir_size(&{
            let solo =
                std::env::temp_dir().join(format!("koi-disk-solo-test-{}", std::process::id()));
            std::fs::create_dir_all(&solo).unwrap();
            let mut sf = std::fs::File::create(solo.join("f.bin")).unwrap();
            sf.write_all(&[0u8; 4096]).unwrap();
            drop(sf);
            solo
        });

        // Two hardlinked entries pointing at the same inode should measure
        // the same total as a single file of that size, not double.
        assert_eq!(measured, single_file_size, "hardlinked file counted twice");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
