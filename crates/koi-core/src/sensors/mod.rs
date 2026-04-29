//! Hardware sensors — CPU/GPU temperatures and fan speeds.
//!
//! Cross-platform abstraction. Backends:
//!
//! - **Linux**: `/sys/class/thermal/thermal_zone*/temp` + sysinfo.
//! - **macOS**: Apple SMC via IOKit. Stub only here — real read requires
//!   `mach` + IOKit bindings or the `smc` crate. Deferred as a follow-up
//!   because it needs testing on Mac hardware.
//! - **Windows**: OHM (OpenHardwareMonitor) WMI bridge. Not yet implemented.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorReading {
    pub sensor: String,
    pub kind: SensorKind,
    pub value: f32,
    pub unit: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SensorKind {
    Temperature,
    FanRpm,
    PowerWatts,
}

/// Read all available sensors. Empty slice = platform not yet supported or no
/// sensors reachable (e.g. no thermal zones exported).
pub fn read_all() -> Vec<SensorReading> {
    #[cfg(target_os = "linux")]
    {
        linux::read_thermal_zones()
    }
    #[cfg(target_os = "macos")]
    {
        macos::read_smc_stub()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        vec![]
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs;

    pub fn read_thermal_zones() -> Vec<SensorReading> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir("/sys/class/thermal") else {
            return out;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("thermal_zone") {
                continue;
            }
            let path = entry.path();
            let type_path = path.join("type");
            let temp_path = path.join("temp");
            let label = fs::read_to_string(&type_path)
                .ok()
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| name.to_string());
            let Ok(raw) = fs::read_to_string(&temp_path) else {
                continue;
            };
            let Ok(millis) = raw.trim().parse::<i32>() else {
                continue;
            };
            out.push(SensorReading {
                sensor: label,
                kind: SensorKind::Temperature,
                value: millis as f32 / 1000.0,
                unit: "°C",
            });
        }
        out
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    // Real SMC read requires IOKit; deferred as a follow-up.
    // Returning empty keeps the monitor alive on Mac but with no sensor data.
    pub fn read_smc_stub() -> Vec<SensorReading> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_all_does_not_panic() {
        let _ = read_all();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_thermal_reads_something_on_modern_hardware() {
        // Allowed to be empty in sandboxed test envs. Just shape-check.
        let readings = read_all();
        for r in &readings {
            assert_eq!(r.unit, "°C");
            assert_eq!(r.kind, SensorKind::Temperature);
        }
    }
}
