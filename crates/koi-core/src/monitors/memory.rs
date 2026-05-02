//! MemoryMonitor — RAM and swap pressure via sysinfo, plus Linux PSI and
//! systemd-oomd readout when available.
//!
//! Cross-platform via sysinfo for RAM/swap. PSI and oomd are Linux-only and
//! gated by `cfg(target_os = "linux")`.

use chrono::Utc;
use sysinfo::System;

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

const WARN_PCT: f64 = 70.0;
const CRIT_PCT: f64 = 85.0;
const SWAP_WARN_PCT: f64 = 50.0;
const PSI_WARN: f64 = 10.0;
const PSI_CRIT: f64 = 25.0;

pub struct MemoryMonitor;

impl Default for MemoryMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryMonitor {
    pub fn new() -> Self {
        Self
    }
}

impl Monitor for MemoryMonitor {
    fn name(&self) -> &'static str {
        "MemoryMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let mut sys = System::new();
        sys.refresh_memory();

        let total = sys.total_memory();
        let used = sys.used_memory();
        let swap_total = sys.total_swap();
        let swap_used = sys.used_swap();
        let mem_pct = pct(used, total);
        let swap_pct = pct(swap_used, swap_total);

        let psi = read_psi_memory();

        let mut status = HealthStatus::Healthy;
        let mut observations = vec![
            Observation {
                key: "memory_used_pct".into(),
                value: serde_json::json!({ "pct": mem_pct, "used_bytes": used, "total_bytes": total }),
                severity: severity_pct(mem_pct, WARN_PCT, CRIT_PCT),
            },
            Observation {
                key: "swap_used_pct".into(),
                value: serde_json::json!({ "pct": swap_pct, "used_bytes": swap_used, "total_bytes": swap_total }),
                severity: if swap_pct >= SWAP_WARN_PCT {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            },
        ];

        if let Some(ref p) = psi {
            observations.push(Observation {
                key: "psi_memory".into(),
                value: serde_json::to_value(p).unwrap_or(serde_json::Value::Null),
                severity: severity_pct(p.some_avg10, PSI_WARN, PSI_CRIT),
            });
        }

        let mut suggestions = Vec::new();
        if mem_pct >= CRIT_PCT {
            status = HealthStatus::Critical;
            suggestions.push(Suggestion {
                message: format!("RAM at {mem_pct:.1}% — memory pressure imminent"),
                severity: Severity::Critical,
                action_hint: Some("Close memory-heavy apps, check leaks".into()),
            });
        } else if mem_pct >= WARN_PCT {
            status = HealthStatus::Warning;
            suggestions.push(Suggestion {
                message: format!("RAM at {mem_pct:.1}% — approaching pressure"),
                severity: Severity::Warning,
                action_hint: None,
            });
        }

        if swap_pct >= SWAP_WARN_PCT {
            if status == HealthStatus::Healthy {
                status = HealthStatus::Warning;
            }
            suggestions.push(Suggestion {
                message: format!("Swap at {swap_pct:.1}% — sustained use indicates RAM shortage"),
                severity: Severity::Warning,
                action_hint: None,
            });
        }

        if let Some(ref p) = psi {
            if p.some_avg10 >= PSI_CRIT || p.full_avg10 >= PSI_CRIT {
                status = HealthStatus::Critical;
                suggestions.push(Suggestion {
                    message: format!(
                        "PSI memory pressure critical (some={:.1}%, full={:.1}%)",
                        p.some_avg10, p.full_avg10
                    ),
                    severity: Severity::Critical,
                    action_hint: Some("Reduce concurrent workload immediately".into()),
                });
            } else if p.some_avg10 >= PSI_WARN {
                if status == HealthStatus::Healthy {
                    status = HealthStatus::Warning;
                }
                suggestions.push(Suggestion {
                    message: format!(
                        "PSI memory pressure detected (some_avg10={:.1}%)",
                        p.some_avg10
                    ),
                    severity: Severity::Warning,
                    action_hint: None,
                });
            }
        }

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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PsiMemory {
    pub some_avg10: f64,
    pub some_avg60: f64,
    pub some_avg300: f64,
    pub some_total_us: u64,
    pub full_avg10: f64,
    pub full_avg60: f64,
    pub full_avg300: f64,
    pub full_total_us: u64,
}

#[cfg(target_os = "linux")]
fn read_psi_memory() -> Option<PsiMemory> {
    let text = std::fs::read_to_string("/proc/pressure/memory").ok()?;
    let mut some = [0f64; 3];
    let mut full = [0f64; 3];
    let mut some_total = 0u64;
    let mut full_total = 0u64;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(kind) = parts.next() else { continue };
        let target_avg = match kind {
            "some" => &mut some,
            "full" => &mut full,
            _ => continue,
        };
        let target_total = match kind {
            "some" => &mut some_total,
            _ => &mut full_total,
        };
        for field in parts {
            let Some((k, v)) = field.split_once('=') else {
                continue;
            };
            match k {
                "avg10" => target_avg[0] = v.parse().unwrap_or(0.0),
                "avg60" => target_avg[1] = v.parse().unwrap_or(0.0),
                "avg300" => target_avg[2] = v.parse().unwrap_or(0.0),
                "total" => *target_total = v.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    Some(PsiMemory {
        some_avg10: some[0],
        some_avg60: some[1],
        some_avg300: some[2],
        some_total_us: some_total,
        full_avg10: full[0],
        full_avg60: full[1],
        full_avg300: full[2],
        full_total_us: full_total,
    })
}

#[cfg(not(target_os = "linux"))]
fn read_psi_memory() -> Option<PsiMemory> {
    None
}

fn pct(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64) * 100.0
    }
}

fn severity_pct(v: f64, warn: f64, crit: f64) -> Severity {
    if v >= crit {
        Severity::Critical
    } else if v >= warn {
        Severity::Warning
    } else {
        Severity::Info
    }
}
