//! ModelSizeMonitor — warns before an oversized local LLM model file can
//! blow past systemd-oomd's memory-pressure limit and freeze the machine.
//!
//! Prevention counterpart to MemoryMonitor's reaction: this watches known
//! model stores (Ollama blobs, HuggingFace hub cache, loose *.gguf in home)
//! and flags any file whose size alone, added to a typical desktop workload,
//! would exceed a RAM-derived fit ceiling. See INC-KOI016 — a 23GB Ollama
//! quant on 32GB RAM triggered exactly this failure mode.
//!
//! Never deletes anything — proposal/notification only (user files tier).

use chrono::Utc;
use std::path::{Path, PathBuf};
use sysinfo::System;
use walkdir::WalkDir;

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

/// Fraction of total RAM a single model file may occupy before koi warns.
/// Matches the host.yaml-derived rule of thumb (file + KV cache + inference
/// overhead must fit alongside normal desktop workload under oomd's 80%
/// pressure limit) with headroom built in rather than assumed at exactly 80%.
const CEILING_RATIO: f64 = 0.6;

/// Below this, a model file isn't worth walking stores for.
const SIZE_FLOOR_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelFile {
    pub path: String,
    pub size_bytes: u64,
    pub store: String,
}

pub struct ModelSizeMonitor {
    home: PathBuf,
}

impl Default for ModelSizeMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelSizeMonitor {
    pub fn new() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Self { home }
    }

    fn watched_stores(&self) -> Vec<(PathBuf, &'static str)> {
        vec![
            (
                PathBuf::from("/usr/share/ollama/.ollama/models/blobs"),
                "ollama-system",
            ),
            (self.home.join(".ollama/models/blobs"), "ollama-user"),
            (self.home.join(".cache/huggingface/hub"), "huggingface-hub"),
        ]
    }

    fn scan(&self) -> Vec<ModelFile> {
        let mut found = Vec::new();

        for (dir, store) in self.watched_stores() {
            if !dir.exists() {
                continue;
            }
            found.extend(scan_dir(&dir, store));
        }

        // Loose *.gguf anywhere directly under $HOME (not recursing into every
        // subdirectory — that would be far too slow within the monitor budget;
        // the known stores above cover the actual accumulation points).
        if let Ok(entries) = std::fs::read_dir(&self.home) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("gguf") {
                    if let Ok(meta) = entry.metadata() {
                        if meta.is_file() && meta.len() >= SIZE_FLOOR_BYTES {
                            found.push(ModelFile {
                                path: path.to_string_lossy().into(),
                                size_bytes: meta.len(),
                                store: "loose-gguf".into(),
                            });
                        }
                    }
                }
            }
        }

        found
    }

    fn ceiling_bytes(&self) -> u64 {
        let mut sys = System::new();
        sys.refresh_memory();
        let total = sys.total_memory(); // bytes, per sysinfo
        ((total as f64) * CEILING_RATIO) as u64
    }
}

fn scan_dir(dir: &Path, store: &'static str) -> Vec<ModelFile> {
    WalkDir::new(dir)
        .follow_links(false)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            (meta.len() >= SIZE_FLOOR_BYTES).then_some(ModelFile {
                path: e.path().to_string_lossy().into(),
                size_bytes: meta.len(),
                store: store.into(),
            })
        })
        .collect()
}

impl Monitor for ModelSizeMonitor {
    fn name(&self) -> &'static str {
        "ModelSizeMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let ceiling = self.ceiling_bytes();
        let files = self.scan();

        let mut over_ceiling: Vec<&ModelFile> =
            files.iter().filter(|f| f.size_bytes > ceiling).collect();
        over_ceiling.sort_by_key(|f| std::cmp::Reverse(f.size_bytes));

        let status = if !over_ceiling.is_empty() {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };

        let observations: Vec<Observation> = files
            .iter()
            .map(|f| Observation {
                key: f.path.clone(),
                value: serde_json::json!({
                    "gib": (f.size_bytes as f64) / (1024f64.powi(3)),
                    "store": f.store,
                    "ceiling_gib": (ceiling as f64) / (1024f64.powi(3)),
                    "over_ceiling": f.size_bytes > ceiling,
                }),
                severity: if f.size_bytes > ceiling {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            })
            .collect();

        let suggestions: Vec<Suggestion> = over_ceiling
            .iter()
            .map(|f| {
                let size_gib = (f.size_bytes as f64) / (1024f64.powi(3));
                let ceiling_gib = (ceiling as f64) / (1024f64.powi(3));
                Suggestion {
                    message: format!(
                        "{} is {:.1} GiB — above the {:.1} GiB fit ceiling for this machine's RAM. \
                         Loading it alongside normal desktop use risks an OOM freeze (see INC-KOI016). \
                         Consider a smaller quant, or run 'koi approve' on the cleanup proposal for this file.",
                        f.path, size_gib, ceiling_gib
                    ),
                    severity: Severity::Warning,
                    action_hint: Some(format!("review before loading: {}", f.path)),
                }
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
