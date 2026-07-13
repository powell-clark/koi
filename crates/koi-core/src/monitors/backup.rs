//! BackupMonitor — report the last result of the encrypted user backup unit.
//!
//! On systemd hosts this reads the user unit state through `systemctl --user
//! show`. Other platforms report the monitor as unavailable without making
//! `koi check` fail.

use chrono::{Local, NaiveDateTime, Utc};

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

const STALE_AFTER_HOURS: i64 = 8 * 24;

/// Derived state of the encrypted backup, from the systemd unit's reported
/// `ActiveState`/`Result` and the age of the last successful exit.
///
/// The key distinction (TASK-KOI192): a `signal` result means the long-running
/// sync was cut short by a reboot or shutdown — this workstation reboots
/// several times a day and a full crypt sync takes far longer than one uptime
/// window — so it is an *interrupted* run that resumes on the next boot, not a
/// failure of the backup command. Whether the data has actually converged on
/// the remote is measured separately by the convergence check in `koi backup`.
#[derive(Debug, PartialEq, Eq)]
enum BackupState {
    Running,
    Interrupted,
    Failed,
    Stale,
    Healthy,
    NeverRan,
}

fn classify_backup(
    active_state: &str,
    result: &str,
    last_success_age_hours: Option<i64>,
) -> BackupState {
    if matches!(active_state, "active" | "activating") {
        BackupState::Running
    } else if result == "signal" {
        BackupState::Interrupted
    } else if !result.is_empty() && result != "success" {
        BackupState::Failed
    } else if let Some(age_hours) = last_success_age_hours {
        if age_hours >= STALE_AFTER_HOURS {
            BackupState::Stale
        } else {
            BackupState::Healthy
        }
    } else {
        BackupState::NeverRan
    }
}

pub struct BackupMonitor;

impl Default for BackupMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl BackupMonitor {
    pub fn new() -> Self {
        Self
    }
}

impl Monitor for BackupMonitor {
    fn name(&self) -> &'static str {
        "BackupMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let Some(properties) = systemd_properties() else {
            return Ok(unavailable_report(self.name(), started));
        };

        let active_state = properties
            .get("ActiveState")
            .map(String::as_str)
            .unwrap_or("");
        let result = properties.get("Result").map(String::as_str).unwrap_or("");
        let exit_timestamp = properties
            .get("ExecMainExitTimestamp")
            .and_then(|value| parse_systemd_timestamp(value));
        let last_success_age_hours =
            exit_timestamp.map(|ts| (Local::now().naive_local() - ts).num_hours().max(0));
        let mut observations = vec![Observation {
            key: "service_state".into(),
            value: serde_json::json!({
                "active_state": active_state,
                "result": result,
                "exit_timestamp": properties.get("ExecMainExitTimestamp"),
            }),
            severity: Severity::Info,
        }];

        let (status, suggestion) = match classify_backup(
            active_state,
            result,
            last_success_age_hours,
        ) {
            BackupState::Running => {
                observations.push(Observation {
                    key: "backup_running".into(),
                    value: serde_json::json!(true),
                    severity: Severity::Info,
                });
                (HealthStatus::Healthy, None)
            }
            BackupState::Interrupted => {
                observations.push(Observation {
                    key: "backup_interrupted".into(),
                    value: serde_json::json!({ "result": result }),
                    severity: Severity::Warning,
                });
                (
                    HealthStatus::Warning,
                    Some(Suggestion {
                        message: "Encrypted backup was interrupted before completing (likely a reboot mid-sync) — it will resume on the next run".into(),
                        severity: Severity::Warning,
                        action_hint: Some("journalctl --user -u koi-backup.service".into()),
                    }),
                )
            }
            BackupState::Failed => (
                HealthStatus::Critical,
                Some(Suggestion {
                    message: format!("Encrypted backup service failed ({result})"),
                    severity: Severity::Critical,
                    action_hint: Some("journalctl --user -u koi-backup.service".into()),
                }),
            ),
            BackupState::Stale => {
                let age_hours = last_success_age_hours.unwrap_or_default();
                observations.push(Observation {
                    key: "last_success_age_hours".into(),
                    value: serde_json::json!(age_hours),
                    severity: Severity::Warning,
                });
                (
                    HealthStatus::Warning,
                    Some(Suggestion {
                        message: format!(
                            "Encrypted backup is stale — last success was {age_hours} hours ago"
                        ),
                        severity: Severity::Warning,
                        action_hint: Some("systemctl --user start koi-backup.service".into()),
                    }),
                )
            }
            BackupState::Healthy => {
                observations.push(Observation {
                    key: "last_success_age_hours".into(),
                    value: serde_json::json!(last_success_age_hours.unwrap_or_default()),
                    severity: Severity::Info,
                });
                (HealthStatus::Healthy, None)
            }
            BackupState::NeverRan => (
                HealthStatus::Warning,
                Some(Suggestion {
                    message: "Encrypted backup has no completed run recorded".into(),
                    severity: Severity::Warning,
                    action_hint: Some("systemctl --user start koi-backup.service".into()),
                }),
            ),
        };

        Ok(MonitorReport {
            monitor: self.name().into(),
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
            collected_at: Utc::now(),
            observations,
            suggestions: suggestion.into_iter().collect(),
        })
    }
}

fn systemd_properties() -> Option<std::collections::HashMap<String, String>> {
    let output = std::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            "koi-backup.service",
            "--no-pager",
            "--property=ActiveState,Result,ExecMainExitTimestamp",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
    )
}

fn parse_systemd_timestamp(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim();
    let timestamp = value.get(4..23)?;
    NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S").ok()
}

fn unavailable_report(name: &str, started: std::time::Instant) -> MonitorReport {
    MonitorReport {
        monitor: name.into(),
        status: HealthStatus::Healthy,
        elapsed_ms: started.elapsed().as_millis() as u64,
        collected_at: Utc::now(),
        observations: vec![Observation {
            key: "service_state".into(),
            value: serde_json::json!({ "available": false }),
            severity: Severity::Info,
        }],
        suggestions: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_systemd_timestamp() {
        assert_eq!(
            parse_systemd_timestamp("Mon 2026-07-13 11:48:41 BST"),
            Some(
                NaiveDateTime::parse_from_str("2026-07-13 11:48:41", "%Y-%m-%d %H:%M:%S").unwrap()
            )
        );
    }

    #[test]
    fn rejects_empty_or_malformed_timestamp() {
        assert!(parse_systemd_timestamp("").is_none());
        assert!(parse_systemd_timestamp("not a timestamp").is_none());
    }

    #[test]
    fn running_states_report_running() {
        assert_eq!(classify_backup("active", "", None), BackupState::Running);
        assert_eq!(
            classify_backup("activating", "signal", None),
            BackupState::Running
        );
    }

    #[test]
    fn signal_kill_is_interrupted_not_failed() {
        // Reboot/shutdown SIGTERM (or SIGKILL) mid-sync — the dominant case on
        // this frequently-rebooted workstation. Must not read as Critical.
        assert_eq!(
            classify_backup("failed", "signal", None),
            BackupState::Interrupted
        );
    }

    #[test]
    fn genuine_non_success_results_are_failed() {
        assert_eq!(
            classify_backup("failed", "exit-code", None),
            BackupState::Failed
        );
        assert_eq!(
            classify_backup("failed", "oom-kill", None),
            BackupState::Failed
        );
    }

    #[test]
    fn success_age_drives_healthy_vs_stale() {
        assert_eq!(
            classify_backup("inactive", "success", Some(1)),
            BackupState::Healthy
        );
        assert_eq!(
            classify_backup("inactive", "success", Some(STALE_AFTER_HOURS)),
            BackupState::Stale
        );
    }

    #[test]
    fn no_recorded_run_is_never_ran() {
        assert_eq!(classify_backup("inactive", "", None), BackupState::NeverRan);
    }
}
