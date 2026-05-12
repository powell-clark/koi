//! WezTermMonitor — process family observability.
//!
//! Scope: count running wezterm processes, sum RSS, identify the top memory
//! hog, report GPU VRAM via nvidia-smi, and detect crashes via the kernel
//! journal (OOM kills and segfaults).
//!
//! Crash detection shells to `journalctl -k` and looks for kernel-level
//! evidence (OOM kill, segfault). Clean exits (window close, logout, reboot)
//! produce no such kernel messages, so false-positive rate is low.
//!
//! Generalises via [`process_family_stats`] so sibling monitors (Ghostty,
//! Chrome, etc.) can reuse the same machinery.
//!
//! **RSS-summation caveat**: `total_rss` sums every matching process's RSS,
//! which double-counts shared library pages. Thresholds below intentionally
//! compare against the top single process, not the sum, to avoid false
//! positives on systems where the family shares a lot of code pages.

use chrono::Utc;
use sysinfo::System;

use crate::{
    monitor::Monitor,
    monitors::wezterm_crash::query_wezterm_crashes,
    state::{self, NewCrashEvent},
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

const WARN_TOP_GIB: f64 = 2.0;
const CRIT_TOP_GIB: f64 = 4.0;

pub struct WezTermMonitor;

impl Default for WezTermMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl WezTermMonitor {
    pub fn new() -> Self {
        Self
    }
}

impl Monitor for WezTermMonitor {
    fn name(&self) -> &'static str {
        "WezTermMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let stats = process_family_stats("wezterm");

        let total_gib = (stats.total_rss as f64) / (1024f64.powi(3));
        let top_gib = stats
            .top
            .as_ref()
            .map(|t| (t.rss as f64) / (1024f64.powi(3)))
            .unwrap_or(0.0);

        // Threshold against the top process, not the sum (see RSS-summation caveat).
        let status = if top_gib >= CRIT_TOP_GIB {
            HealthStatus::Critical
        } else if top_gib >= WARN_TOP_GIB {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };

        let gpu_vram_mb = query_nvidia_smi_vram(&stats.pids);

        // Detect and persist new crashes from the kernel journal. Best-effort:
        // failures are swallowed so the rest of the report still completes.
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
                    "WezTerm had {crash_count} crash(es) in the last 24 h — run `koi history wezterm` for details"
                ),
                severity: Severity::Warning,
                action_hint: Some("Check crash type (oom_kill vs segfault) to determine root cause".into()),
            });
        }
        if top_gib >= CRIT_TOP_GIB {
            suggestions.push(Suggestion {
                message: format!(
                    "WezTerm top process using {top_gib:.1} GiB RSS — investigate runaway pane"
                ),
                severity: Severity::Critical,
                action_hint: Some("Identify and close the pane running the heavy workload".into()),
            });
        } else if top_gib >= WARN_TOP_GIB {
            suggestions.push(Suggestion {
                message: format!("WezTerm top process RSS at {top_gib:.1} GiB"),
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

/// Query nvidia-smi for VRAM used by the given PIDs.
///
/// Returns `None` when nvidia-smi is not installed or fails (e.g. no NVIDIA
/// driver). Returns `Some(0)` when the driver is present but none of the
/// supplied PIDs appear in the compute-apps list. Returns `Some(n)` with the
/// summed VRAM in MiB when matching entries are found.
pub fn query_nvidia_smi_vram(pids: &[u32]) -> Option<u64> {
    if pids.is_empty() {
        return Some(0);
    }
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=pid,used_gpu_memory",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    Some(parse_nvidia_smi_vram(&text, pids))
}

/// Detect WezTerm crashes from the kernel journal and persist new ones to
/// SQLite. Returns the count of crash entries recorded in the last 24 hours
/// (including any newly detected ones). Best-effort: returns 0 on any error.
fn detect_and_persist_crashes(current_rss_gib: f64) -> u64 {
    let Ok(db_path) = state::default_db_path() else {
        return 0;
    };
    let Ok(conn) = state::open(&db_path) else {
        return 0;
    };

    if let Some(new_crashes) = query_wezterm_crashes(24) {
        let last_rss_mb = Some(current_rss_gib * 1024.0);
        for crash in &new_crashes {
            let ev = NewCrashEvent {
                comm: "wezterm-gui".into(),
                detected_at: Utc::now(),
                crash_type: crash.crash_type.clone(),
                pid: crash.pid,
                last_rss_mb,
                message: crash.message.clone(),
            };
            let _ = state::record_process_crash(&conn, &ev);
        }
    }

    // Count distinct crashes in the last 24 hours from the DB.
    conn.query_row(
        "SELECT COUNT(*) FROM process_crashes WHERE comm = 'wezterm-gui'
         AND detected_at >= datetime('now', '-24 hours')",
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0) as u64
}

/// Parse the CSV output of `nvidia-smi --query-compute-apps=pid,used_gpu_memory
/// --format=csv,noheader,nounits` and sum VRAM in MiB for the given PIDs.
///
/// Lines that do not parse cleanly are skipped without error.
pub fn parse_nvidia_smi_vram(output: &str, pids: &[u32]) -> u64 {
    let pid_set: std::collections::HashSet<u32> = pids.iter().copied().collect();
    let mut total_mb: u64 = 0;
    for line in output.lines() {
        let mut parts = line.splitn(2, ',');
        let Some(pid_str) = parts.next() else {
            continue;
        };
        let Some(mb_str) = parts.next() else {
            continue;
        };
        let Ok(pid) = pid_str.trim().parse::<u32>() else {
            continue;
        };
        let mb: u64 = mb_str.trim().parse().unwrap_or(0);
        if pid_set.contains(&pid) {
            total_mb += mb;
        }
    }
    total_mb
}

/// Aggregated stats for a process family matched by name fragment.
#[derive(Debug, Clone, Default)]
pub struct FamilyStats {
    pub count: usize,
    pub total_rss: u64,
    pub top: Option<ProcessInfo>,
    /// All PIDs in the family, used for GPU VRAM attribution.
    pub pids: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub rss: u64,
}

pub fn process_family_stats(name_fragment: &str) -> FamilyStats {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let mut stats = FamilyStats::default();
    let fragment_lower = name_fragment.to_ascii_lowercase();

    for (pid, proc_) in sys.processes() {
        let name = proc_.name().to_string_lossy().to_ascii_lowercase();
        if !name.contains(&fragment_lower) {
            continue;
        }
        let rss = proc_.memory();
        stats.count += 1;
        stats.total_rss += rss;
        stats.pids.push(pid.as_u32());
        match &stats.top {
            Some(top) if top.rss >= rss => {}
            _ => {
                stats.top = Some(ProcessInfo {
                    pid: pid.as_u32(),
                    name: proc_.name().to_string_lossy().into_owned(),
                    rss,
                });
            }
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_stats_returns_without_panic() {
        // Process called "init" reliably exists on most systems. Test just
        // checks the function's shape — zero is an acceptable result.
        let _ = process_family_stats("kworker");
    }

    #[test]
    fn unknown_family_returns_zero_stats() {
        let s = process_family_stats("definitely-not-a-real-process-xyz");
        assert_eq!(s.count, 0);
        assert_eq!(s.total_rss, 0);
        assert!(s.top.is_none());
        assert!(s.pids.is_empty());
    }

    #[test]
    fn parse_nvidia_smi_vram_sums_matching_pids() {
        let output = "4727, 1556\n49527, 48\n12345, 200\n";
        let pids = [49527u32, 12345];
        let mb = parse_nvidia_smi_vram(output, &pids);
        assert_eq!(mb, 248); // 48 + 200
    }

    #[test]
    fn parse_nvidia_smi_vram_no_match_returns_zero() {
        let output = "4727, 1556\n";
        let pids = [99999u32];
        assert_eq!(parse_nvidia_smi_vram(output, &pids), 0);
    }

    #[test]
    fn parse_nvidia_smi_vram_empty_output_returns_zero() {
        assert_eq!(parse_nvidia_smi_vram("", &[1u32, 2]), 0);
    }

    #[test]
    fn parse_nvidia_smi_vram_malformed_lines_skipped() {
        // A line without a comma and a line with a non-numeric PID are skipped.
        let output = "not-a-number, 100\n4727, 1556\n";
        let pids = [4727u32];
        assert_eq!(parse_nvidia_smi_vram(output, &pids), 1556);
    }

    #[test]
    fn parse_nvidia_smi_vram_empty_pids_returns_zero_via_query() {
        // query_nvidia_smi_vram with empty pids short-circuits before shelling out.
        assert_eq!(query_nvidia_smi_vram(&[]), Some(0));
    }

    #[test]
    fn family_stats_collects_all_pids() {
        // Non-existent process — pids should be empty.
        let s = process_family_stats("definitely-not-a-real-process-xyz");
        assert!(s.pids.is_empty());
    }
}
