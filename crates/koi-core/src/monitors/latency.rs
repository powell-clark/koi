//! LatencyMonitor — TCP connect latency to configurable targets.
//!
//! Uses blocking `TcpStream::connect_timeout` so there's no async dependency
//! here; the
//! daemon wraps these calls in `spawn_blocking`. History across calls is
//! reconstructed from SQLite `monitor_reports` by downstream analysers.

use chrono::Utc;
use std::{
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

const TIMEOUT: Duration = Duration::from_secs(2);
const WARN_MS: u64 = 100;
const CRIT_MS: u64 = 300;

const TARGETS: &[(&str, &str, u16)] = &[
    ("Cloudflare DNS", "1.1.1.1", 53),
    ("Google DNS", "8.8.8.8", 53),
];

pub struct LatencyMonitor;

impl Default for LatencyMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyMonitor {
    pub fn new() -> Self {
        Self
    }
}

impl Monitor for LatencyMonitor {
    fn name(&self) -> &'static str {
        "LatencyMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = Instant::now();
        let mut observations = Vec::new();
        let mut max_latency_ms: Option<u64> = None;
        let mut unreachable_count = 0usize;

        for (name, host, port) in TARGETS {
            let latency = probe(host, *port);
            match latency {
                Ok(ms) => {
                    max_latency_ms = Some(max_latency_ms.map_or(ms, |m| m.max(ms)));
                    observations.push(Observation {
                        key: (*name).into(),
                        value: serde_json::json!({ "latency_ms": ms, "host": host, "port": port }),
                        severity: if ms >= CRIT_MS {
                            Severity::Critical
                        } else if ms >= WARN_MS {
                            Severity::Warning
                        } else {
                            Severity::Info
                        },
                    });
                }
                Err(e) => {
                    unreachable_count += 1;
                    observations.push(Observation {
                        key: (*name).into(),
                        value: serde_json::json!({
                            "latency_ms": null,
                            "host": host,
                            "port": port,
                            "error": e,
                        }),
                        severity: Severity::Warning,
                    });
                }
            }
        }

        let status = if unreachable_count == TARGETS.len() {
            HealthStatus::Critical
        } else if unreachable_count > 0 || max_latency_ms.is_some_and(|m| m >= WARN_MS) {
            // CRIT_MS >= WARN_MS, so >= WARN_MS subsumes the critical-latency case here;
            // latency alone never escalates past Warning — only all-targets-unreachable is Critical.
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };

        let mut suggestions = vec![];
        if unreachable_count == TARGETS.len() {
            suggestions.push(Suggestion {
                message: "All latency targets unreachable — check network connection".into(),
                severity: Severity::Critical,
                action_hint: Some("ip a; ping 1.1.1.1".into()),
            });
        } else if let Some(ms) = max_latency_ms {
            if ms >= CRIT_MS {
                suggestions.push(Suggestion {
                    message: format!("Network latency high ({ms} ms) — investigate ISP or wifi"),
                    severity: Severity::Warning,
                    action_hint: None,
                });
            }
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

fn probe(host: &str, port: u16) -> std::result::Result<u64, String> {
    let addr_str = format!("{host}:{port}");
    let mut addrs = addr_str
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}: {e}"))?;
    let addr: SocketAddr = addrs
        .next()
        .ok_or_else(|| "no address resolved".to_string())?;

    let started = Instant::now();
    match TcpStream::connect_timeout(&addr, TIMEOUT) {
        Ok(_) => Ok(started.elapsed().as_millis() as u64),
        Err(e) => Err(format!("connect {addr}: {e}")),
    }
}
