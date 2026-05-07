//! PackageMonitor — count outdated apt / npm / pip packages.
//!
//! A 24-hour JSON cache keeps the cost of shelling to package managers off the
//! health-check hot path. Linux
//! scans apt, macOS scans brew; npm and pip scan cross-platform. Windows winget
//! is planned.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

const TTL_SECS: u64 = 24 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutdatedPackage {
    pub manager: String,
    pub name: String,
    pub current: String,
    pub latest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    timestamp: u64,
    packages: Vec<OutdatedPackage>,
}

pub struct PackageMonitor {
    cache_path: PathBuf,
}

impl PackageMonitor {
    pub fn new() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| crate::error::Error::Config("$HOME not set".into()))?;
        let cache_path = home.join(".cache/koi/package-monitor.json");
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self { cache_path })
    }

    fn packages(&self) -> Vec<OutdatedPackage> {
        if let Some(cached) = self.read_cache() {
            if unix_now().saturating_sub(cached.timestamp) < TTL_SECS {
                return cached.packages;
            }
        }
        let pkgs = scan_all();
        let _ = self.write_cache(&pkgs);
        pkgs
    }

    fn read_cache(&self) -> Option<CacheFile> {
        serde_json::from_slice(&fs::read(&self.cache_path).ok()?).ok()
    }

    fn write_cache(&self, pkgs: &[OutdatedPackage]) -> Result<()> {
        let payload = CacheFile {
            timestamp: unix_now(),
            packages: pkgs.to_vec(),
        };
        let tmp = self.cache_path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&payload)?)?;
        fs::rename(tmp, &self.cache_path)?;
        Ok(())
    }
}

impl Monitor for PackageMonitor {
    fn name(&self) -> &'static str {
        "PackageMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let pkgs = self.packages();
        let by_manager = group_by_manager(&pkgs);

        let apt = by_manager.get("apt").map_or(0, |v| v.len());
        let brew = by_manager.get("brew").map_or(0, |v| v.len());
        let npm = by_manager.get("npm").map_or(0, |v| v.len());
        let pip = by_manager.get("pip").map_or(0, |v| v.len());

        let status = if apt > 10 || brew > 10 || pkgs.len() > 50 {
            HealthStatus::Critical
        } else if !pkgs.is_empty() {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };

        let observations = vec![
            Observation {
                key: "total_outdated".into(),
                value: serde_json::json!({ "count": pkgs.len() }),
                severity: if pkgs.is_empty() {
                    Severity::Info
                } else {
                    Severity::Warning
                },
            },
            Observation {
                key: "apt".into(),
                value: serde_json::json!({ "count": apt }),
                severity: if apt > 10 {
                    Severity::Critical
                } else if apt > 0 {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            },
            Observation {
                key: "brew".into(),
                value: serde_json::json!({ "count": brew }),
                severity: if brew > 10 {
                    Severity::Critical
                } else if brew > 0 {
                    Severity::Warning
                } else {
                    Severity::Info
                },
            },
            Observation {
                key: "npm".into(),
                value: serde_json::json!({ "count": npm }),
                severity: Severity::Info,
            },
            Observation {
                key: "pip".into(),
                value: serde_json::json!({ "count": pip }),
                severity: Severity::Info,
            },
        ];

        let mut suggestions = vec![];
        if apt > 0 {
            suggestions.push(Suggestion {
                message: format!("{apt} apt package(s) upgradable — `sudo apt upgrade`"),
                severity: if apt > 10 {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                action_hint: Some("sudo apt update && sudo apt upgrade".into()),
            });
        }
        if brew > 0 {
            suggestions.push(Suggestion {
                message: format!("{brew} Homebrew package(s) outdated — `brew upgrade`"),
                severity: if brew > 10 {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                action_hint: Some("brew update && brew upgrade".into()),
            });
        }
        if npm > 0 {
            suggestions.push(Suggestion {
                message: format!("{npm} global npm package(s) outdated"),
                severity: Severity::Info,
                action_hint: Some("npm -g update".into()),
            });
        }
        if pip > 0 {
            suggestions.push(Suggestion {
                message: format!("{pip} pip package(s) outdated"),
                severity: Severity::Info,
                action_hint: Some("pip list --outdated".into()),
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

fn scan_all() -> Vec<OutdatedPackage> {
    let mut out = vec![];
    #[cfg(target_os = "linux")]
    out.extend(scan_apt());
    #[cfg(target_os = "macos")]
    out.extend(scan_brew());
    out.extend(scan_npm());
    out.extend(scan_pip());
    out
}

#[cfg(target_os = "linux")]
fn scan_apt() -> Vec<OutdatedPackage> {
    let Some(stdout) = run("apt", &["list", "--upgradable"]) else {
        return vec![];
    };
    stdout
        .lines()
        .filter(|l| l.contains("/") && !l.starts_with("Listing"))
        .filter_map(|l| {
            // format: "pkg/repo new-version arch [upgradable from: old-version]"
            let mut it = l.split_whitespace();
            let name_repo = it.next()?;
            let latest = it.next()?.to_string();
            let _arch = it.next();
            let from_idx = l.find("from: ")?;
            let current = l[from_idx + 6..].trim_end_matches(']').to_string();
            Some(OutdatedPackage {
                manager: "apt".into(),
                name: name_repo.split('/').next()?.to_string(),
                current,
                latest,
            })
        })
        .collect()
}

fn scan_npm() -> Vec<OutdatedPackage> {
    let Some(stdout) = run("npm", &["-g", "outdated", "--json"]) else {
        return vec![];
    };
    let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&stdout)
    else {
        return vec![];
    };
    map.iter()
        .filter_map(|(name, v)| {
            Some(OutdatedPackage {
                manager: "npm".into(),
                name: name.clone(),
                current: v.get("current")?.as_str()?.to_string(),
                latest: v.get("latest")?.as_str()?.to_string(),
            })
        })
        .collect()
}

fn scan_pip() -> Vec<OutdatedPackage> {
    let Some(stdout) = run("pip", &["list", "--outdated", "--format=json"]) else {
        return vec![];
    };
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) else {
        return vec![];
    };
    arr.iter()
        .filter_map(|v| {
            Some(OutdatedPackage {
                manager: "pip".into(),
                name: v.get("name")?.as_str()?.to_string(),
                current: v.get("version")?.as_str()?.to_string(),
                latest: v.get("latest_version")?.as_str()?.to_string(),
            })
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn scan_brew() -> Vec<OutdatedPackage> {
    let Some(stdout) = run("brew", &["outdated", "--json=v2"]) else {
        return vec![];
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&stdout) else {
        return vec![];
    };
    // v2 splits results into "formulae" and "casks"; both carry name,
    // installed_versions (array, newest last) and current_version.
    let mut out = vec![];
    for key in ["formulae", "casks"] {
        let Some(arr) = root.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for v in arr {
            let Some(name) = v.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let current = v
                .get("installed_versions")
                .and_then(|iv| iv.as_array())
                .and_then(|a| a.last())
                .and_then(|s| s.as_str())
                .unwrap_or("?")
                .to_string();
            let latest = v
                .get("current_version")
                .and_then(|c| c.as_str())
                .unwrap_or("?")
                .to_string();
            out.push(OutdatedPackage {
                manager: "brew".into(),
                name: name.to_string(),
                current,
                latest,
            });
        }
    }
    out
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    // apt list exits 0 even with stderr warnings; don't demand success
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn group_by_manager(
    pkgs: &[OutdatedPackage],
) -> std::collections::HashMap<String, Vec<&OutdatedPackage>> {
    let mut out: std::collections::HashMap<String, Vec<&OutdatedPackage>> =
        std::collections::HashMap::new();
    for p in pkgs {
        out.entry(p.manager.clone()).or_default().push(p);
    }
    out
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
