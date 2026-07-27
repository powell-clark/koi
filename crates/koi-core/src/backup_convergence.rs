//! Backup convergence — has the encrypted remote caught up with the local
//! source?
//!
//! The original success model ("one `koi-backup.service` run exited 0") is
//! unreachable on this workstation: it reboots several times a day and a full
//! crypt sync takes far longer than one uptime window, so systemd SIGTERMs the
//! run at `shutdown.target` every time (TASK-KOI192). `rclone sync` is
//! inherently resumable — it skips files already present on the remote — so
//! real progress accrues across reboots even though no single run ever ends
//! cleanly.
//!
//! Convergence measures that accrued progress directly: compare the remote's
//! byte total against the locally-filtered byte total. `rclone size` on a crypt
//! remote reports *decrypted* sizes, so the two are directly comparable.
//!
//! The measurement needs a network round-trip and so cannot run inside
//! `BackupMonitor`'s 200ms budget. `koi backup --status` performs it and
//! persists a small snapshot; `BackupMonitor` reads that snapshot cheaply.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{state::default_data_dir, Result};

/// The remote counts as converged once it holds at least this fraction of the
/// local filtered byte total. Not 1.0: the source is a live working tree that
/// changes throughout a multi-day sync, so exact equality is never observed.
pub const CONVERGED_RATIO: f64 = 0.99;

/// A snapshot older than this no longer describes the current state — the
/// source will have drifted. Re-measure rather than trust it.
pub const SNAPSHOT_FRESH_HOURS: i64 = 7 * 24;

/// One convergence measurement, persisted so the fast monitor can read it
/// without paying for a network call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvergenceSnapshot {
    pub measured_at: DateTime<Utc>,
    pub local_bytes: u64,
    pub remote_bytes: u64,
    pub ratio: f64,
    pub converged: bool,
}

impl ConvergenceSnapshot {
    pub fn new(local_bytes: u64, remote_bytes: u64, measured_at: DateTime<Utc>) -> Self {
        let ratio = convergence_ratio(local_bytes, remote_bytes);
        Self {
            measured_at,
            local_bytes,
            remote_bytes,
            ratio,
            converged: is_converged(ratio),
        }
    }

    /// Progress as a whole percentage, capped at 100 for display.
    pub fn percent(&self) -> u64 {
        ((self.ratio * 100.0).round() as i64).clamp(0, 100) as u64
    }
}

/// What the persisted snapshot says about the backup right now.
#[derive(Debug, PartialEq, Eq)]
pub enum ConvergenceState {
    /// Remote holds essentially everything the local source holds.
    Converged,
    /// Real progress on the remote, but not yet caught up.
    Converging,
    /// A measurement exists but is too old to describe the current source.
    SnapshotStale,
    /// No measurement has ever been taken.
    NeverMeasured,
}

/// Remote bytes as a fraction of local bytes.
///
/// An empty source is vacuously converged — there is nothing left to copy —
/// which also keeps the caller from dividing by zero.
pub fn convergence_ratio(local_bytes: u64, remote_bytes: u64) -> f64 {
    if local_bytes == 0 {
        return 1.0;
    }
    remote_bytes as f64 / local_bytes as f64
}

pub fn is_converged(ratio: f64) -> bool {
    ratio >= CONVERGED_RATIO
}

/// Classify a (possibly absent) snapshot as of `now`.
pub fn classify_convergence(
    snapshot: Option<&ConvergenceSnapshot>,
    now: DateTime<Utc>,
) -> ConvergenceState {
    let Some(snapshot) = snapshot else {
        return ConvergenceState::NeverMeasured;
    };
    if now - snapshot.measured_at >= Duration::hours(SNAPSHOT_FRESH_HOURS) {
        return ConvergenceState::SnapshotStale;
    }
    if snapshot.converged {
        ConvergenceState::Converged
    } else {
        ConvergenceState::Converging
    }
}

/// Total bytes reported by `rclone size --json`, whose output is
/// `{"count":N,"bytes":N}`.
pub fn parse_rclone_size_bytes(stdout: &str) -> Result<u64> {
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())?;
    parsed
        .get("bytes")
        .and_then(|bytes| bytes.as_u64())
        .ok_or_else(|| {
            crate::Error::Config("rclone size --json had no numeric `bytes` field".into())
        })
}

pub fn snapshot_path() -> Result<PathBuf> {
    Ok(default_data_dir()?.join("backup-convergence.json"))
}

pub fn write_snapshot(snapshot: &ConvergenceSnapshot) -> Result<()> {
    let path = snapshot_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(snapshot)?)?;
    Ok(())
}

/// Read the persisted snapshot. A missing or unreadable file is not an error —
/// it simply means no measurement has been taken, which `classify_convergence`
/// reports as `NeverMeasured`.
pub fn read_snapshot() -> Option<ConvergenceSnapshot> {
    let path = snapshot_path().ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hours_ago: i64) -> DateTime<Utc> {
        Utc::now() - Duration::hours(hours_ago)
    }

    #[test]
    fn ratio_is_remote_over_local() {
        assert_eq!(convergence_ratio(100, 50), 0.5);
        assert_eq!(convergence_ratio(100, 100), 1.0);
    }

    #[test]
    fn empty_source_is_vacuously_converged() {
        // Nothing to copy: must not divide by zero, must not read as 0% done.
        assert_eq!(convergence_ratio(0, 0), 1.0);
        assert!(is_converged(convergence_ratio(0, 0)));
    }

    #[test]
    fn remote_ahead_of_local_still_counts_as_converged() {
        // Orphans the never-completing sync has not deleted yet can push the
        // remote above the local total. That is still "everything is there".
        assert!(is_converged(convergence_ratio(100, 120)));
    }

    #[test]
    fn tolerance_admits_live_working_tree_drift() {
        // A multi-day sync over a changing source never hits exactly 1.0.
        assert!(is_converged(0.995));
        assert!(!is_converged(0.98));
    }

    #[test]
    fn snapshot_derives_ratio_and_converged_flag() {
        let snapshot = ConvergenceSnapshot::new(1000, 500, Utc::now());
        assert_eq!(snapshot.ratio, 0.5);
        assert!(!snapshot.converged);
        assert_eq!(snapshot.percent(), 50);
    }

    #[test]
    fn percent_is_capped_at_one_hundred() {
        let snapshot = ConvergenceSnapshot::new(100, 200, Utc::now());
        assert_eq!(snapshot.percent(), 100);
    }

    #[test]
    fn absent_snapshot_is_never_measured() {
        assert_eq!(
            classify_convergence(None, Utc::now()),
            ConvergenceState::NeverMeasured
        );
    }

    #[test]
    fn fresh_snapshot_reports_converged_or_converging() {
        let converged = ConvergenceSnapshot::new(100, 100, at(1));
        assert_eq!(
            classify_convergence(Some(&converged), Utc::now()),
            ConvergenceState::Converged
        );

        let converging = ConvergenceSnapshot::new(100, 40, at(1));
        assert_eq!(
            classify_convergence(Some(&converging), Utc::now()),
            ConvergenceState::Converging
        );
    }

    #[test]
    fn old_snapshot_is_stale_even_when_it_said_converged() {
        // The source drifts; a week-old "converged" reading proves nothing now.
        let snapshot = ConvergenceSnapshot::new(100, 100, at(SNAPSHOT_FRESH_HOURS));
        assert_eq!(
            classify_convergence(Some(&snapshot), Utc::now()),
            ConvergenceState::SnapshotStale
        );
    }

    #[test]
    fn parses_rclone_size_json() {
        assert_eq!(
            parse_rclone_size_bytes(r#"{"count":320055,"bytes":32985348833}"#).unwrap(),
            32_985_348_833
        );
        // rclone pads its output with a trailing newline.
        assert_eq!(
            parse_rclone_size_bytes("{\"count\":1,\"bytes\":42}\n").unwrap(),
            42
        );
    }

    #[test]
    fn rejects_rclone_output_without_bytes() {
        assert!(parse_rclone_size_bytes(r#"{"count":5}"#).is_err());
        assert!(parse_rclone_size_bytes("not json").is_err());
    }
}
