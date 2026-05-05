//! DockerMonitor — shells out to docker CLI for disk usage + dangling resources.
//!
//! No daemon-level bollard dependency yet (see ADR-0013) — this version
//! preserves the CLI-shelling
//! behaviour for maximal portability across WSL/Docker Desktop/colima. A native
//! bollard-based version arrives when the daemon is wired.

use chrono::Utc;
use std::{process::Command, time::Duration};

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

const RECLAIMABLE_WARN_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB
const RECLAIMABLE_CRIT_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GiB
const DANGLING_IMG_CRIT: usize = 5;

#[derive(Default, Debug)]
struct Totals {
    images_total: u32,
    images_active: u32,
    images_reclaimable: u64,
    volumes_total: u32,
    volumes_active: u32,
    volumes_reclaimable: u64,
    build_cache_bytes: u64,
}

pub struct DockerMonitor;

impl Default for DockerMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl DockerMonitor {
    pub fn new() -> Self {
        Self
    }
}

impl Monitor for DockerMonitor {
    fn name(&self) -> &'static str {
        "DockerMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();

        if !docker_reachable() {
            return Ok(MonitorReport {
                monitor: self.name().into(),
                status: HealthStatus::Healthy,
                elapsed_ms: started.elapsed().as_millis() as u64,
                collected_at: Utc::now(),
                observations: vec![Observation {
                    key: "docker_available".into(),
                    value: serde_json::json!(false),
                    severity: Severity::Info,
                }],
                suggestions: vec![],
            });
        }

        let totals = read_system_df().unwrap_or_default();
        let dangling_imgs = list_lines(&["images", "-f", "dangling=true", "--format", "{{.ID}}"]);
        let dangling_vols = list_lines(&[
            "volume",
            "ls",
            "-f",
            "dangling=true",
            "--format",
            "{{.Name}}",
        ]);
        let stopped = list_lines(&["ps", "-a", "-f", "status=exited", "--format", "{{.Names}}"]);

        let reclaimable = totals.images_reclaimable + totals.volumes_reclaimable;
        let reclaimable_gib = (reclaimable as f64) / (1024f64.powi(3));

        let status =
            if reclaimable >= RECLAIMABLE_CRIT_BYTES || dangling_imgs.len() >= DANGLING_IMG_CRIT {
                HealthStatus::Critical
            } else if reclaimable >= RECLAIMABLE_WARN_BYTES
                || !dangling_imgs.is_empty()
                || !dangling_vols.is_empty()
                || stopped.len() > 3
            {
                HealthStatus::Warning
            } else {
                HealthStatus::Healthy
            };

        let observations = vec![
            Observation {
                key: "reclaimable_gib".into(),
                value: serde_json::json!({ "gib": reclaimable_gib, "bytes": reclaimable }),
                severity: if reclaimable >= RECLAIMABLE_WARN_BYTES {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            },
            Observation {
                key: "dangling_images".into(),
                value: serde_json::json!({ "count": dangling_imgs.len(), "sample": &dangling_imgs[..dangling_imgs.len().min(5)] }),
                severity: if dangling_imgs.is_empty() {
                    Severity::Info
                } else {
                    Severity::Warning
                },
            },
            Observation {
                key: "dangling_volumes".into(),
                value: serde_json::json!({ "count": dangling_vols.len(), "sample": &dangling_vols[..dangling_vols.len().min(5)] }),
                severity: if dangling_vols.is_empty() {
                    Severity::Info
                } else {
                    Severity::Warning
                },
            },
            Observation {
                key: "stopped_containers".into(),
                value: serde_json::json!({ "count": stopped.len(), "sample": &stopped[..stopped.len().min(5)] }),
                severity: if stopped.len() > 3 {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            },
            Observation {
                key: "build_cache_gib".into(),
                value: serde_json::json!({ "gib": (totals.build_cache_bytes as f64) / (1024f64.powi(3)) }),
                severity: Severity::Info,
            },
        ];

        let mut suggestions = vec![];
        if !dangling_imgs.is_empty() {
            suggestions.push(Suggestion {
                message: format!(
                    "Remove {} dangling image(s): docker image prune",
                    dangling_imgs.len()
                ),
                severity: Severity::Warning,
                action_hint: Some("docker image prune -f".into()),
            });
        }
        if !dangling_vols.is_empty() {
            suggestions.push(Suggestion {
                message: format!(
                    "Remove {} dangling volume(s): docker volume prune (inspect first!)",
                    dangling_vols.len()
                ),
                severity: Severity::Warning,
                action_hint: Some("docker volume ls -f dangling=true".into()),
            });
        }
        if reclaimable >= RECLAIMABLE_WARN_BYTES {
            suggestions.push(Suggestion {
                message: format!(
                    "{:.1} GiB reclaimable — docker system prune -a --volumes",
                    reclaimable_gib
                ),
                severity: Severity::Warning,
                action_hint: Some("docker system prune -a --volumes".into()),
            });
        }

        Ok(MonitorReport {
            monitor: self.name().into(),
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
            collected_at: Utc::now(),
            observations,
            suggestions,
        })
    }
}

fn docker_reachable() -> bool {
    let output = Command::new("docker").arg("info").output();
    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

fn read_system_df() -> Option<Totals> {
    let out = run_docker(&[
        "system",
        "df",
        "--format",
        "{{.Type}}\t{{.TotalCount}}\t{{.Active}}\t{{.Size}}\t{{.Reclaimable}}",
    ])?;
    let mut totals = Totals::default();
    for line in out.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 5 {
            continue;
        }
        let kind = parts[0].to_ascii_lowercase();
        let total: u32 = parts[1].parse().unwrap_or(0);
        let active: u32 = parts[2].parse().unwrap_or(0);
        let size = parse_docker_size(parts[3]);
        let reclaimable = parse_docker_size(parts[4].split_whitespace().next().unwrap_or("0"));
        match kind.as_str() {
            "images" => {
                totals.images_total = total;
                totals.images_active = active;
                totals.images_reclaimable = reclaimable;
                let _ = size;
            }
            "local volumes" => {
                totals.volumes_total = total;
                totals.volumes_active = active;
                totals.volumes_reclaimable = reclaimable;
            }
            "build cache" => {
                totals.build_cache_bytes = size;
            }
            _ => {}
        }
    }
    Some(totals)
}

fn list_lines(args: &[&str]) -> Vec<String> {
    run_docker(args)
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn run_docker(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("docker");
    cmd.args(args);
    let output = cmd.output().ok()?;
    let _ = Duration::from_secs(10); // command-level timeouts need async; acceptable for MVP
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_docker_size(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() || s == "0B" {
        return 0;
    }
    let (num_part, unit_part) = s.split_at(
        s.char_indices()
            .find(|(_, c)| c.is_alphabetic())
            .map(|(i, _)| i)
            .unwrap_or(s.len()),
    );
    let num: f64 = num_part.parse().unwrap_or(0.0);
    let mult: f64 = match unit_part.to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" => 1024.0,
        "MB" => 1024f64.powi(2),
        "GB" => 1024f64.powi(3),
        "TB" => 1024f64.powi(4),
        _ => 1.0,
    };
    (num * mult) as u64
}
