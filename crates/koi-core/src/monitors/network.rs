//! NetworkMonitor — interface counters via sysinfo.
//!
//! Throughput-over-time (bytes/sec) needs two snapshots separated by an
//! interval; that belongs in the
//! daemon's stateful path, not a single run() invocation. This monitor surfaces
//! the absolute counters (bytes_sent/recv, errors, drops) — enough for the
//! tray UI and report, and enough to derive deltas downstream from SQLite
//! history if the daemon persists on cadence.

use chrono::Utc;
use sysinfo::Networks;

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

pub struct NetworkMonitor;

impl Default for NetworkMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkMonitor {
    pub fn new() -> Self {
        Self
    }
}

impl Monitor for NetworkMonitor {
    fn name(&self) -> &'static str {
        "NetworkMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let mut networks = Networks::new_with_refreshed_list();
        networks.refresh(true);

        let mut total_rx: u64 = 0;
        let mut total_tx: u64 = 0;
        let mut total_errors_in: u64 = 0;
        let mut total_errors_out: u64 = 0;
        let mut observations = Vec::new();

        for (name, data) in &networks {
            // Skip loopback and virtual interfaces without traffic.
            if name == "lo" || name.starts_with("docker") || name.starts_with("veth") {
                continue;
            }
            let rx = data.total_received();
            let tx = data.total_transmitted();
            let err_in = data.total_errors_on_received();
            let err_out = data.total_errors_on_transmitted();
            total_rx += rx;
            total_tx += tx;
            total_errors_in += err_in;
            total_errors_out += err_out;

            observations.push(Observation {
                key: name.clone(),
                value: serde_json::json!({
                    "rx_bytes_total": rx,
                    "tx_bytes_total": tx,
                    "rx_errors": err_in,
                    "tx_errors": err_out,
                }),
                severity: if err_in + err_out > 0 {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            });
        }

        observations.insert(
            0,
            Observation {
                key: "totals".into(),
                value: serde_json::json!({
                    "rx_gib": (total_rx as f64) / (1024f64.powi(3)),
                    "tx_gib": (total_tx as f64) / (1024f64.powi(3)),
                    "errors_in": total_errors_in,
                    "errors_out": total_errors_out,
                }),
                severity: Severity::Info,
            },
        );

        let status = if total_errors_in + total_errors_out > 100 {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };

        let mut suggestions = vec![];
        if total_errors_in + total_errors_out > 100 {
            suggestions.push(Suggestion {
                message: format!(
                    "{} network errors detected (in: {}, out: {}) — inspect driver/cable",
                    total_errors_in + total_errors_out,
                    total_errors_in,
                    total_errors_out
                ),
                severity: Severity::Warning,
                action_hint: Some("ip -s link show".into()),
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
