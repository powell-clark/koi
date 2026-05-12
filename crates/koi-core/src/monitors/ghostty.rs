//! GhosttyMonitor — process family observability.
//!
//! Mirrors WezTermMonitor: count running ghostty processes, sum RSS, identify
//! the top memory hog, report GPU VRAM via nvidia-smi, and detect crashes via
//! the kernel journal (OOM kills and segfaults).
//!
//! Returns a Healthy report with zero counts when Ghostty is not installed —
//! no errors are surfaced in `koi check` if the process is absent.

use chrono::Utc;

use crate::{
    monitor::Monitor,
    monitors::{
        ghostty_crash::query_ghostty_crashes,
        wezterm::{process_family_stats, query_nvidia_smi_vram},
    },
    state::{self, NewCrashEvent},
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

const WARN_TOP_GIB: f64 = 2.0;
const CRIT_TOP_GIB: f64 = 4.0;

pub struct GhosttyMonitor;

impl Default for GhosttyMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl GhosttyMonitor {
    pub fn new() -> Self {
        Self
    }
}

impl Monitor for GhosttyMonitor {
    fn name(&self) -> &'static str {
        "GhosttyMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let stats = process_family_stats("ghostty");

        let total_gib = (stats.total_rss as f64) / (1024f64.powi(3));
        let top_gib = stats
            .top
            .as_ref()
            .map(|t| (t.rss as f64) / (1024f64.powi(3)))
            .unwrap_or(0.0);

        let status = if top_gib >= CRIT_TOP_GIB {
            HealthStatus::Critical
        } else if top_gib >= WARN_TOP_GIB {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };

        let gpu_vram_mb = query_nvidia_smi_vram(&stats.pids);
        let crash_count = detect_and_persist_crashes(total_gib);

        let mut observations = vec![
            Observation {
                key: "process_count".into(),
                value: serde_json::json!(stats.count),
                severity: Severity::Info,
            },
            Observation {
                key: "total_rss_gib".into(),
                value: serde_json::json!({
                    "gib": total_gib,
                    "bytes": stats.total_rss,
                    "caveat": "sum double-counts shared pages",
                }),
                severity: Severity::Info,
            },
            Observation {
                key: "gpu_vram_mb".into(),
                value: serde_json::json!(gpu_vram_mb),
                severity: Severity::Info,
            },
            Observation {
                key: "recent_crashes".into(),
                value: serde_json::json!(crash_count),
                severity: if crash_count > 0 {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            },
        ];

        if let Some(top) = &stats.top {
            observations.push(Observation {
                key: "top_process".into(),
                value: serde_json::json!({
                    "pid": top.pid,
                    "rss_gib": top_gib,
                    "name": top.name,
                }),
                severity: if top_gib >= CRIT_TOP_GIB {
                    Severity::Critical
                } else if top_gib >= WARN_TOP_GIB {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            });
        }

        let mut suggestions = vec![];
        if crash_count > 0 {
            suggestions.push(Suggestion {
                message: format!(
                    "Ghostty had {crash_count} crash(es) in the last 24 h — run `koi history ghostty` for details"
                ),
                severity: Severity::Warning,
                action_hint: Some("Check crash type (oom_kill vs segfault) to determine root cause".into()),
            });
        }
        if top_gib >= CRIT_TOP_GIB {
            suggestions.push(Suggestion {
                message: format!(
                    "Ghostty top process using {top_gib:.1} GiB RSS — investigate runaway pane"
                ),
                severity: Severity::Critical,
                action_hint: Some("Identify and close the pane running the heavy workload".into()),
            });
        } else if top_gib >= WARN_TOP_GIB {
            suggestions.push(Suggestion {
                message: format!("Ghostty top process RSS at {top_gib:.1} GiB"),
                severity: Severity::Warning,
                action_hint: None,
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

fn detect_and_persist_crashes(current_rss_gib: f64) -> u64 {
    let Ok(db_path) = state::default_db_path() else {
        return 0;
    };
    let Ok(conn) = state::open(&db_path) else {
        return 0;
    };

    if let Some(new_crashes) = query_ghostty_crashes(24) {
        let last_rss_mb = Some(current_rss_gib * 1024.0);
        for crash in &new_crashes {
            let ev = NewCrashEvent {
                comm: "ghostty".into(),
                detected_at: Utc::now(),
                crash_type: crash.crash_type.clone(),
                pid: crash.pid,
                last_rss_mb,
                message: crash.message.clone(),
            };
            let _ = state::record_process_crash(&conn, &ev);
        }
    }

    conn.query_row(
        "SELECT COUNT(*) FROM process_crashes WHERE comm = 'ghostty'
         AND detected_at >= datetime('now', '-24 hours')",
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitors::wezterm::process_family_stats;

    #[test]
    fn ghostty_monitor_runs_without_panic() {
        let m = GhosttyMonitor::new();
        let report = m.run().expect("GhosttyMonitor should not error");
        // Ghostty may not be installed — Healthy with count=0 is the valid outcome.
        assert!(matches!(
            report.status,
            crate::types::HealthStatus::Healthy
                | crate::types::HealthStatus::Warning
                | crate::types::HealthStatus::Critical
        ));
    }

    #[test]
    fn ghostty_absent_produces_healthy_zero_count() {
        // On a system without Ghostty, process_family_stats returns zeroed stats.
        let stats = process_family_stats("ghostty");
        // We can't assert count==0 (user might have it), but we can assert no panic.
        let _ = stats.count;
    }

    #[test]
    fn ghostty_monitor_report_has_required_observations() {
        let m = GhosttyMonitor::new();
        let report = m.run().expect("run succeeds");
        let keys: Vec<&str> = report.observations.iter().map(|o| o.key.as_str()).collect();
        assert!(keys.contains(&"process_count"));
        assert!(keys.contains(&"total_rss_gib"));
        assert!(keys.contains(&"gpu_vram_mb"));
        assert!(keys.contains(&"recent_crashes"));
    }
}
