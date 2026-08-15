//! CacheMonitor — known developer caches (npm, Playwright, pnpm, uv, etc.),
//! with staleness detection and clean-command hints.
//!
//! Uses a 6-hour JSON cache for size snapshots since walking e.g. `.cache/uv`
//! can take seconds.

use crate::{
    fs_size::dir_size,
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};
use chrono::Utc;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const STALE_DAYS: u64 = 30;
const CRIT_STALE_COUNT: usize = 3;
const CRIT_STALE_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GiB
const TTL_SECS: u64 = 6 * 3600;

// (display name, $HOME-relative path, clean-command hint). Linux/XDG layout.
#[cfg(not(target_os = "macos"))]
const KNOWN_CACHES: &[(&str, &str, &str)] = &[
    ("npm", ".npm", "npm cache clean --force"),
    (
        "Playwright",
        ".cache/ms-playwright",
        "rm -rf ~/.cache/ms-playwright",
    ),
    (
        "pre-commit",
        ".cache/pre-commit",
        "rm -rf ~/.cache/pre-commit",
    ),
    (
        "TypeScript",
        ".cache/typescript",
        "rm -rf ~/.cache/typescript",
    ),
    ("puppeteer", ".cache/puppeteer", "rm -rf ~/.cache/puppeteer"),
    (
        "Cypress",
        ".cache/Cypress",
        "rm -rf ~/.cache/Cypress ~/.config/Cypress",
    ),
    ("pip", ".cache/pip", "pip cache purge"),
    ("Docker buildx", ".docker/buildx", "docker buildx prune"),
    ("uv", ".cache/uv", "uv cache clean"),
    ("pnpm", ".local/share/pnpm/store", "pnpm store prune"),
];

// macOS layout: most tools cache under ~/Library/Caches. npm keeps ~/.npm;
// command-based cleaners (pip/uv/pnpm/docker/npm) are path-independent.
#[cfg(target_os = "macos")]
const KNOWN_CACHES: &[(&str, &str, &str)] = &[
    ("npm", ".npm", "npm cache clean --force"),
    (
        "Playwright",
        "Library/Caches/ms-playwright",
        "rm -rf ~/Library/Caches/ms-playwright",
    ),
    (
        "pre-commit",
        "Library/Caches/pre-commit",
        "rm -rf ~/Library/Caches/pre-commit",
    ),
    (
        "TypeScript",
        "Library/Caches/typescript",
        "rm -rf ~/Library/Caches/typescript",
    ),
    (
        "puppeteer",
        "Library/Caches/puppeteer",
        "rm -rf ~/Library/Caches/puppeteer",
    ),
    (
        "Cypress",
        "Library/Caches/Cypress",
        "rm -rf ~/Library/Caches/Cypress",
    ),
    ("pip", "Library/Caches/pip", "pip cache purge"),
    ("Docker buildx", ".docker/buildx", "docker buildx prune"),
    ("uv", "Library/Caches/uv", "uv cache clean"),
    ("pnpm", "Library/pnpm/store", "pnpm store prune"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub last_access_days: i64,
    pub stale: bool,
    pub clean_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    timestamp: u64,
    entries: Vec<CacheEntry>,
}

pub struct CacheMonitor {
    home: PathBuf,
    cache_path: PathBuf,
}

impl CacheMonitor {
    pub fn new() -> Result<Self> {
        let home = crate::state::home_dir()?;
        let cache_path = home.join(".cache/koi/cache-monitor.json");
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self { home, cache_path })
    }

    fn entries(&self) -> Vec<CacheEntry> {
        if let Some(cached) = self.read_cache() {
            if unix_now().saturating_sub(cached.timestamp) < TTL_SECS {
                return cached.entries;
            }
        }
        let fresh = self.scan();
        let _ = self.write_cache(&fresh);
        fresh
    }

    fn scan(&self) -> Vec<CacheEntry> {
        let targets: Vec<_> = KNOWN_CACHES
            .iter()
            .filter_map(|(name, rel, cmd)| {
                let p = self.home.join(rel);
                p.exists().then_some((*name, p, *cmd))
            })
            .collect();

        targets
            .par_iter()
            .map(|(name, path, cmd)| {
                let size_bytes = dir_size(path);
                let last_access_days = access_age_days(path).unwrap_or(-1);
                let stale = last_access_days >= 0 && (last_access_days as u64) >= STALE_DAYS;
                CacheEntry {
                    name: (*name).into(),
                    path: path.to_string_lossy().into(),
                    size_bytes,
                    last_access_days,
                    stale,
                    clean_command: (*cmd).into(),
                }
            })
            .collect()
    }

    fn read_cache(&self) -> Option<CacheFile> {
        serde_json::from_slice(&fs::read(&self.cache_path).ok()?).ok()
    }

    fn write_cache(&self, entries: &[CacheEntry]) -> Result<()> {
        let payload = CacheFile {
            timestamp: unix_now(),
            entries: entries.to_vec(),
        };
        let tmp = self.cache_path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&payload)?)?;
        fs::rename(tmp, &self.cache_path)?;
        Ok(())
    }
}

impl Monitor for CacheMonitor {
    fn name(&self) -> &'static str {
        "CacheMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let entries = self.entries();
        let stale: Vec<&CacheEntry> = entries.iter().filter(|e| e.stale).collect();
        let stale_bytes: u64 = stale.iter().map(|e| e.size_bytes).sum();

        let status = if stale.len() >= CRIT_STALE_COUNT || stale_bytes >= CRIT_STALE_BYTES {
            HealthStatus::Critical
        } else if !stale.is_empty() {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };

        let observations = entries
            .iter()
            .map(|e| Observation {
                key: e.name.clone(),
                value: serde_json::json!({
                    "gib": (e.size_bytes as f64) / (1024f64.powi(3)),
                    "last_access_days": e.last_access_days,
                    "stale": e.stale,
                }),
                severity: if e.stale {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            })
            .collect();

        let mut stale_sorted: Vec<&CacheEntry> = stale.clone();
        stale_sorted.sort_by_key(|e| std::cmp::Reverse(e.size_bytes));
        let suggestions = stale_sorted
            .iter()
            .take(3)
            .map(|e| Suggestion {
                message: format!(
                    "Clean {} ({:.1} GiB, {} days idle): {}",
                    e.name,
                    (e.size_bytes as f64) / (1024f64.powi(3)),
                    e.last_access_days,
                    e.clean_command
                ),
                severity: Severity::Warning,
                action_hint: Some(e.clean_command.clone()),
            })
            .collect();

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

fn access_age_days(path: &Path) -> Option<i64> {
    let meta = fs::metadata(path).ok()?;
    let atime = meta
        .accessed()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    let age_secs = unix_now().saturating_sub(atime) as i64;
    Some(age_secs / 86_400)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
