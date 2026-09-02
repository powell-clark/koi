//! Assert koi's own systemd units are alive (TASK-KOI232).
//!
//! koi monitors the machine but did not monitor itself.
//! `koi-screenshot-relay.path` sat dead for five days with nothing noticing
//! (TASK-KOI231), and `koi-audit-quick.service` was in failed state throughout
//! that same investigation, also undetected. A `.path` unit that hits its start
//! limit stops watching permanently and says nothing; the only signal is a
//! behaviour the operator eventually notices by hand.
//!
//! # Two states, not one
//!
//! Reporting only `failed` would have caught neither incident cleanly. A timer
//! or path unit is *supposed* to sit `active (waiting)`; one that is merely
//! `inactive` is not failed but is also not watching anything, which is the
//! silent failure this monitor exists for. [`classify_unit`] separates the two.
//!
//! # Report, never repair
//!
//! Nothing here restarts a unit. A unit that fails repeatedly needs to be seen,
//! and a monitor that silently bounces it converts a visible fault into a
//! recurring invisible one.

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

/// What `systemctl` reported about one unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitState {
    pub name: String,
    pub load: String,
    pub active: String,
    pub sub: String,
}

/// The verdict on one unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitVerdict {
    /// Failed, or its unit file could not be loaded.
    Failed,
    /// A timer or path unit that should be waiting but is not — the silent
    /// case TASK-KOI231 was five days late catching.
    NotWatching,
    /// A oneshot service between runs. Normal.
    IdleOneshot,
    Healthy,
}

/// Units that are supposed to sit active-waiting rather than run continuously.
fn is_watcher(name: &str) -> bool {
    name.ends_with(".timer") || name.ends_with(".path")
}

pub fn classify_unit(unit: &UnitState) -> UnitVerdict {
    if unit.load == "not-found" || unit.load == "error" || unit.active == "failed" {
        return UnitVerdict::Failed;
    }
    if is_watcher(&unit.name) {
        // "active waiting" is the healthy resting state for these; anything
        // else means nothing is being watched.
        return if unit.active == "active" && (unit.sub == "waiting" || unit.sub == "running") {
            UnitVerdict::Healthy
        } else {
            UnitVerdict::NotWatching
        };
    }
    if unit.active == "inactive" {
        // A .service between timer firings. Not a fault.
        return UnitVerdict::IdleOneshot;
    }
    UnitVerdict::Healthy
}

/// Parse `systemctl --user list-units --all 'koi*' --no-legend --no-pager`.
///
/// Deliberately tolerant: systemd pads columns and prefixes a marker on
/// not-found units, and a parser that panics on an odd row would take the whole
/// health check down with it.
pub fn parse_list_units(output: &str) -> Vec<UnitState> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim_start_matches(['●', '*', ' ']).trim();
            if line.is_empty() {
                return None;
            }
            let mut fields = line.split_whitespace();
            let name = fields.next()?.to_string();
            if !name.contains('.') {
                return None;
            }
            Some(UnitState {
                name,
                load: fields.next().unwrap_or_default().to_string(),
                active: fields.next().unwrap_or_default().to_string(),
                sub: fields.next().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

fn read_units() -> Option<Vec<UnitState>> {
    let out = std::process::Command::new("systemctl")
        .args([
            "--user",
            "list-units",
            "--all",
            "koi*",
            "--no-legend",
            "--no-pager",
        ])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_list_units(&String::from_utf8_lossy(&out.stdout)))
}

pub struct UnitMonitor;

impl Default for UnitMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl UnitMonitor {
    pub fn new() -> Self {
        Self
    }
}

impl Monitor for UnitMonitor {
    fn name(&self) -> &'static str {
        "UnitMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let mut observations = Vec::new();
        let mut suggestions = Vec::new();
        let mut status = HealthStatus::Healthy;

        // No systemd (macOS, Windows, a container) is not a fault — it is a
        // platform where this check does not apply.
        let Some(units) = read_units() else {
            observations.push(Observation {
                key: "units.available".to_string(),
                value: serde_json::json!(false),
                severity: Severity::Info,
            });
            return Ok(MonitorReport {
                monitor: "UnitMonitor".to_string(),
                status,
                elapsed_ms: started.elapsed().as_millis() as u64,
                collected_at: chrono::Utc::now(),
                observations,
                suggestions,
            });
        };

        let (mut failed, mut not_watching) = (0usize, 0usize);
        for unit in &units {
            match classify_unit(unit) {
                UnitVerdict::Failed => {
                    failed += 1;
                    status = HealthStatus::Critical;
                    suggestions.push(Suggestion {
                        message: format!(
                            "{} is {} ({}). Inspect it — koi does not restart its own units.",
                            unit.name, unit.active, unit.sub
                        ),
                        severity: Severity::Critical,
                        action_hint: Some(format!("systemctl --user status {}", unit.name)),
                    });
                }
                UnitVerdict::NotWatching => {
                    not_watching += 1;
                    if status == HealthStatus::Healthy {
                        status = HealthStatus::Warning;
                    }
                    suggestions.push(Suggestion {
                        message: format!(
                            "{} is {} ({}) — it should be active (waiting). Nothing is being watched or scheduled.",
                            unit.name, unit.active, unit.sub
                        ),
                        severity: Severity::Warning,
                        action_hint: Some(format!("systemctl --user start {}", unit.name)),
                    });
                }
                UnitVerdict::IdleOneshot | UnitVerdict::Healthy => {}
            }
        }

        observations.push(Observation {
            key: "units.total".to_string(),
            value: serde_json::json!(units.len()),
            severity: Severity::Info,
        });
        observations.push(Observation {
            key: "units.failed".to_string(),
            value: serde_json::json!(failed),
            severity: if failed > 0 {
                Severity::Critical
            } else {
                Severity::Info
            },
        });
        observations.push(Observation {
            key: "units.not_watching".to_string(),
            value: serde_json::json!(not_watching),
            severity: if not_watching > 0 {
                Severity::Warning
            } else {
                Severity::Info
            },
        });

        Ok(MonitorReport {
            monitor: "UnitMonitor".to_string(),
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
            collected_at: chrono::Utc::now(),
            observations,
            suggestions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(name: &str, load: &str, active: &str, sub: &str) -> UnitState {
        UnitState {
            name: name.to_string(),
            load: load.to_string(),
            active: active.to_string(),
            sub: sub.to_string(),
        }
    }

    #[test]
    fn a_failed_service_is_failed() {
        assert_eq!(
            classify_unit(&unit(
                "koi-audit-quick.service",
                "loaded",
                "failed",
                "failed"
            )),
            UnitVerdict::Failed
        );
    }

    #[test]
    fn a_dead_path_unit_is_caught_even_though_it_is_not_failed() {
        // The TASK-KOI231 incident exactly: koi-screenshot-relay.path stopped
        // watching and sat inactive for five days. Reporting only `failed`
        // would have missed it, which is why NotWatching exists.
        assert_eq!(
            classify_unit(&unit(
                "koi-screenshot-relay.path",
                "loaded",
                "inactive",
                "dead"
            )),
            UnitVerdict::NotWatching
        );
    }

    #[test]
    fn a_waiting_timer_is_healthy() {
        assert_eq!(
            classify_unit(&unit("koi-backup.timer", "loaded", "active", "waiting")),
            UnitVerdict::Healthy
        );
    }

    #[test]
    fn a_oneshot_service_between_runs_is_not_a_fault() {
        // Most koi services are timer-driven and sit inactive by design.
        // Flagging these would make the check noise and get it ignored.
        assert_eq!(
            classify_unit(&unit(
                "koi-health-check.service",
                "loaded",
                "inactive",
                "dead"
            )),
            UnitVerdict::IdleOneshot
        );
    }

    #[test]
    fn a_unit_file_that_vanished_is_failed_not_idle() {
        assert_eq!(
            classify_unit(&unit("koi-gone.service", "not-found", "inactive", "dead")),
            UnitVerdict::Failed
        );
    }

    #[test]
    fn a_running_service_is_healthy() {
        assert_eq!(
            classify_unit(&unit(
                "koi-network-indicator.service",
                "loaded",
                "active",
                "running"
            )),
            UnitVerdict::Healthy
        );
    }

    #[test]
    fn parses_real_systemctl_output_including_the_failure_marker() {
        // Recorded from this host, plus a failed row carrying systemd's dot.
        let out = "\
  koi-screenshot-relay.path        loaded active     waiting       Watch for new screenshots
  koi-health-check.service         loaded inactive   dead          Koi System Health Check
● koi-audit-quick.service          loaded failed     failed        Koi security audit
  koi-backup.timer                 loaded active     waiting       Weekly encrypted backup
";
        let units = parse_list_units(out);
        assert_eq!(units.len(), 4);
        assert_eq!(units[2].name, "koi-audit-quick.service");
        assert_eq!(units[2].active, "failed");
        assert_eq!(classify_unit(&units[2]), UnitVerdict::Failed);
        assert_eq!(classify_unit(&units[0]), UnitVerdict::Healthy);
    }

    #[test]
    fn a_malformed_row_is_skipped_rather_than_panicking() {
        // The check runs inside the health check; a parser that panics on an
        // odd row would take the whole thing down.
        let units = parse_list_units("\n   \nnot-a-unit-line\nkoi-x.timer loaded active waiting\n");
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].name, "koi-x.timer");
    }

    #[test]
    fn the_monitor_runs_inside_its_budget_on_this_host() {
        let m = UnitMonitor::new();
        let report = m.run().expect("must not fail even without systemd");
        assert!(
            report.elapsed_ms < m.budget_ms(),
            "took {}ms against {}ms",
            report.elapsed_ms,
            m.budget_ms()
        );
    }
}
