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
            let state = if age >= CACHE_TTL_SECS {
                CacheState::Stale
            } else {
                CacheState::Fresh
            };
            return (cache.sizes, state);
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

        let _ = cache_state;
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
