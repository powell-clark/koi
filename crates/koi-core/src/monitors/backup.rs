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
        let mut observations = vec![Observation {
            key: "service_state".into(),
            value: serde_json::json!({
                "active_state": active_state,
                "result": result,
                "exit_timestamp": properties.get("ExecMainExitTimestamp"),
            }),
            severity: Severity::Info,
        }];

        let (status, suggestion) = if matches!(active_state, "active" | "activating") {
            observations.push(Observation {
                key: "backup_running".into(),
                value: serde_json::json!(true),
                severity: Severity::Info,
            });
            (HealthStatus::Healthy, None)
        } else if !result.is_empty() && result != "success" {
            (
                HealthStatus::Critical,
                Some(Suggestion {
                    message: format!("Encrypted backup service failed ({result})"),
                    severity: Severity::Critical,
                    action_hint: Some("journalctl --user -u koi-backup.service".into()),
                }),
            )
        } else if let Some(exit_timestamp) = exit_timestamp {
            let age_hours = (Local::now().naive_local() - exit_timestamp)
                .num_hours()
                .max(0);
            observations.push(Observation {
                key: "last_success_age_hours".into(),
                value: serde_json::json!(age_hours),
                severity: if age_hours >= STALE_AFTER_HOURS {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            });
            if age_hours >= STALE_AFTER_HOURS {
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
            } else {
                (HealthStatus::Healthy, None)
            }
        } else {
            (
                HealthStatus::Warning,
                Some(Suggestion {
                    message: "Encrypted backup has no completed run recorded".into(),
                    severity: Severity::Warning,
                    action_hint: Some("systemctl --user start koi-backup.service".into()),
                }),
            )
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
}
