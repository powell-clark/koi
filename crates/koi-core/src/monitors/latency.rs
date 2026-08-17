//! LatencyMonitor — TCP connect latency to configurable targets.
//!
//! Probe method: a blocking `TcpStream::connect_timeout` to each target's
//! IP on **port 443** (both anycast resolvers also serve HTTPS). Port 443 is
//! deliberate: VPNs that guard against DNS leaks (e.g. Mullvad WireGuard)
//! block or hijack third-party traffic on port 53, so probing 53 through a
//! tunnel reports every target unreachable while the connection is in fact
//! healthy (TASK-KOI175). Port 443 passes through such tunnels unmodified,
//! so "all targets unreachable" genuinely means offline, tunnel or not.
//!
//! Targets are raw IPs, so no DNS resolution is on the probe path. When an
//! active tunnel interface (`wg*`/`tun*`) is detected it is reported as an
//! info observation, and named in the all-unreachable suggestion so a
//! tunnel-induced failure is diagnosable at a glance.
//!
//! Blocking is fine here; the daemon wraps these calls in `spawn_blocking`.
//! History across calls is reconstructed from SQLite `monitor_reports` by
//! downstream analysers.

use chrono::Utc;
use std::{
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::Path,
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
    ("Cloudflare (1.1.1.1)", "1.1.1.1", 443),
    ("Google (8.8.8.8)", "8.8.8.8", 443),
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

        let tunnel = active_tunnel(Path::new("/sys/class/net"));
        if let Some(ref iface) = tunnel {
            observations.push(Observation {
                key: "vpn_tunnel".into(),
                value: serde_json::json!({ "interface": iface }),
                severity: Severity::Info,
            });
        }

        let status = derive_status(unreachable_count, TARGETS.len(), max_latency_ms);

        let mut suggestions = vec![];
        if unreachable_count == TARGETS.len() {
            let message = match tunnel {
                Some(ref iface) => format!(
                    "All latency targets unreachable on port 443 — likely offline; \
                     tunnel {iface} is up, so also check the VPN itself"
                ),
                None => "All latency targets unreachable — check network connection".into(),
            };
            suggestions.push(Suggestion {
                message,
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

/// Only all-targets-unreachable is Critical; any partial failure or elevated
/// latency is Warning (CRIT_MS >= WARN_MS, so `>= WARN_MS` subsumes the
/// critical-latency case — latency alone never escalates past Warning).
fn derive_status(unreachable: usize, total: usize, max_latency_ms: Option<u64>) -> HealthStatus {
    if unreachable == total {
        HealthStatus::Critical
    } else if unreachable > 0 || max_latency_ms.is_some_and(|m| m >= WARN_MS) {
        HealthStatus::Warning
    } else {
        HealthStatus::Healthy
    }
}

/// First active tunnel interface (`wg*` or `tun*`) under `net_dir`, if any.
/// WireGuard interfaces report operstate "unknown" rather than "up", so any
/// operstate other than "down" counts as active.
fn active_tunnel(net_dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(net_dir).ok()?;
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("wg") || n.starts_with("tun"))
        .collect();
    names.sort();
    names.into_iter().find(|name| {
        std::fs::read_to_string(net_dir.join(name).join("operstate"))
            .map(|s| s.trim() != "down")
            .unwrap_or(false)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_unreachable_is_critical() {
        assert_eq!(derive_status(2, 2, None), HealthStatus::Critical);
    }

    #[test]
    fn partial_unreachable_is_warning_not_critical() {
        assert_eq!(derive_status(1, 2, Some(20)), HealthStatus::Warning);
    }

    #[test]
    fn high_latency_is_warning_never_critical() {
        assert_eq!(
            derive_status(0, 2, Some(CRIT_MS + 500)),
            HealthStatus::Warning
        );
    }

    #[test]
    fn all_reachable_and_fast_is_healthy() {
        assert_eq!(
            derive_status(0, 2, Some(WARN_MS - 1)),
            HealthStatus::Healthy
        );
    }

    #[test]
    fn targets_probe_https_port_not_dns() {
        // Port 53 to third-party resolvers is blocked by leak-guarding VPNs
        // (Mullvad); the probe port must be 443 (TASK-KOI175).
        for (_, _, port) in TARGETS {
            assert_eq!(*port, 443);
        }
    }

    #[test]
    fn tunnel_detected_when_wg_interface_active() {
        let dir = std::env::temp_dir().join(format!("koi-latency-test-{}", std::process::id()));
        let wg = dir.join("wg0-mullvad");
        std::fs::create_dir_all(&wg).unwrap();
        // WireGuard reports operstate "unknown", not "up" — must still count.
        std::fs::write(wg.join("operstate"), "unknown\n").unwrap();
        let eth = dir.join("enp34s0");
        std::fs::create_dir_all(&eth).unwrap();
        std::fs::write(eth.join("operstate"), "up\n").unwrap();

        assert_eq!(active_tunnel(&dir), Some("wg0-mullvad".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_tunnel_when_wg_interface_down() {
        let dir =
            std::env::temp_dir().join(format!("koi-latency-test-down-{}", std::process::id()));
        let wg = dir.join("wg0");
        std::fs::create_dir_all(&wg).unwrap();
        std::fs::write(wg.join("operstate"), "down\n").unwrap();

        assert_eq!(active_tunnel(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_tunnel_when_directory_missing() {
        assert_eq!(active_tunnel(Path::new("/nonexistent-koi-test")), None);
    }

    #[test]
    fn probe_succeeds_against_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ms = probe("127.0.0.1", port).expect("local probe should connect");
        assert!(ms < 1000);
    }

    #[test]
    fn probe_fails_against_closed_port() {
        // Bind then drop to find a port that is very likely closed.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(probe("127.0.0.1", port).is_err());
    }
}
