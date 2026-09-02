//! Inbox dating and aging (TASK-KOI241, STORY-KOI063, ADR-0018).
//!
//! ADR-0018 made koi the sole owner of `~/inbox/` and named the disciplines
//! that were absent: scoping, aging, dating, anti-accumulation, cross-system
//! sync. This is the dating and aging slice. It moves nothing — it records
//! when koi first saw each item and says how old the pile is getting.
//!
//! # Why first_seen is immutable
//!
//! It is the entire basis of aging. A re-scan that refreshed it would reset
//! every item's age to zero on every run, and an inbox that never appears to
//! accumulate is exactly the failure ADR-0018 was written about: 3.7 GB of
//! un-triaged material that nobody noticed piling up.
//!
//! # Renames do not launder age
//!
//! The pre-mortem's failure mode: a renamed file looks new by path and dodges
//! every warning. Identity is therefore (inode, size) as well as path, so a
//! rename inside the inbox inherits its original date.

use chrono::{DateTime, Duration, Utc};

/// Aging tiers in days. An item past the first is worth mentioning; past the
/// second it is the thing the ADR was written about.
pub const AGING_TIER_NOTICE_DAYS: i64 = 7;
pub const AGING_TIER_STALE_DAYS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeTier {
    Fresh,
    /// Older than the notice tier.
    Ageing,
    /// Older than the stale tier — accumulation, not intake.
    Stale,
}

pub fn classify_age(first_seen: DateTime<Utc>, now: DateTime<Utc>) -> AgeTier {
    let age = now - first_seen;
    if age >= Duration::days(AGING_TIER_STALE_DAYS) {
        AgeTier::Stale
    } else if age >= Duration::days(AGING_TIER_NOTICE_DAYS) {
        AgeTier::Ageing
    } else {
        AgeTier::Fresh
    }
}

pub fn age_days(first_seen: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    (now - first_seen).num_days()
}

/// Whether a directory is claimed by another system, and so must be reported
/// rather than proposed for filing (AC-3).
///
/// koi's own recursive marker on the inbox root does not count: ADR-0018 gives
/// koi ownership there, so treating it as foreign would make koi defer to
/// itself and never triage anything.
pub fn is_foreign_managed_zone(dir: &std::path::Path) -> bool {
    let Some(zone) = crate::filing::managed_zone::load_zone(dir) else {
        return false;
    };
    !zone.system.eq_ignore_ascii_case("koi")
}

// -- the monitor -----------------------------------------------------------

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
};

/// Reports how old the inbox pile is getting. Records first sight as a side
/// effect of looking, which is what makes aging possible at all.
pub struct InboxAgeMonitor {
    root: std::path::PathBuf,
}

impl InboxAgeMonitor {
    pub fn new() -> crate::Result<Self> {
        Ok(Self {
            root: crate::state::home_dir()?.join("inbox"),
        })
    }

    pub fn with_root(root: std::path::PathBuf) -> Self {
        Self { root }
    }
}

impl Monitor for InboxAgeMonitor {
    fn name(&self) -> &'static str {
        "InboxAgeMonitor"
    }

    fn run(&self) -> crate::Result<MonitorReport> {
        use std::os::unix::fs::MetadataExt;
        let started = std::time::Instant::now();
        let now = Utc::now();
        let mut observations = Vec::new();
        let mut suggestions = Vec::new();
        let mut status = HealthStatus::Healthy;

        if !self.root.is_dir() {
            return Ok(MonitorReport {
                monitor: "InboxAgeMonitor".to_string(),
                status,
                elapsed_ms: started.elapsed().as_millis() as u64,
                collected_at: now,
                observations,
                suggestions,
            });
        }

        let conn = crate::state::default_db_path()
            .and_then(|p| crate::state::open(&p))
            .ok();
        let mut seen_paths = Vec::new();
        let (mut ageing, mut stale, mut foreign) = (0usize, 0usize, 0usize);
        let mut oldest_days = 0i64;

        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.filter_map(std::result::Result::ok) {
                let path = entry.path();
                let name = entry.file_name();
                if name.to_string_lossy().starts_with('.') {
                    continue;
                }
                // AC-3: a subdirectory another system claims is reported, never
                // proposed. koi's own recursive marker on the root does not
                // count, or koi would defer to itself and triage nothing.
                if path.is_dir() && is_foreign_managed_zone(&path) {
                    foreign += 1;
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                let path_str = path.display().to_string();
                seen_paths.push(path_str.clone());

                let first_seen = match &conn {
                    Some(c) => crate::state::record_inbox_first_seen(
                        c,
                        &path_str,
                        now,
                        i64::try_from(meta.size()).ok(),
                        i64::try_from(meta.ino()).ok(),
                    )
                    .unwrap_or(now),
                    None => now,
                };

                let days = age_days(first_seen, now);
                oldest_days = oldest_days.max(days);
                match classify_age(first_seen, now) {
                    AgeTier::Stale => {
                        stale += 1;
                        status = HealthStatus::Warning;
                    }
                    AgeTier::Ageing => ageing += 1,
                    AgeTier::Fresh => {}
                }
            }
        }

        // An item that has been filed stops aging; leaving it recorded would
        // report a pile that is no longer there.
        if let Some(c) = &conn {
            let _ = crate::state::forget_inbox_items(c, &seen_paths);
        }

        observations.push(Observation {
            key: "inbox.items".to_string(),
            value: serde_json::json!(seen_paths.len()),
            severity: Severity::Info,
        });
        observations.push(Observation {
            key: "inbox.oldest_days".to_string(),
            value: serde_json::json!(oldest_days),
            severity: if stale > 0 {
                Severity::Warning
            } else {
                Severity::Info
            },
        });
        if foreign > 0 {
            observations.push(Observation {
                key: "inbox.foreign_managed_dirs".to_string(),
                value: serde_json::json!(foreign),
                severity: Severity::Info,
            });
        }

        if stale > 0 {
            suggestions.push(Suggestion {
                message: format!(
                    "{stale} inbox item(s) older than {AGING_TIER_STALE_DAYS} days; oldest is {oldest_days} days. This is accumulation, not intake."
                ),
                severity: Severity::Warning,
                action_hint: Some("koi scan".to_string()),
            });
        } else if ageing > 0 {
            suggestions.push(Suggestion {
                message: format!(
                    "{ageing} inbox item(s) older than {AGING_TIER_NOTICE_DAYS} days; oldest is {oldest_days} days."
                ),
                severity: Severity::Info,
                action_hint: Some("koi scan".to_string()),
            });
        }

        Ok(MonitorReport {
            monitor: "InboxAgeMonitor".to_string(),
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
            collected_at: now,
            observations,
            suggestions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `now` is fixed and every age derives from it. Sampling Utc::now()
    // twice made a 14-day age measure 13 days and 23:59:59.999, which
    // truncates to 13 — a flaky test that would have failed roughly whenever
    // CI was slow between two statements.
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-02T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ago(days: i64) -> DateTime<Utc> {
        now() - Duration::days(days)
    }

    #[test]
    fn tiers_land_on_the_documented_boundaries() {
        let n = now();
        assert_eq!(classify_age(ago(0), n), AgeTier::Fresh);
        assert_eq!(classify_age(ago(6), n), AgeTier::Fresh);
        assert_eq!(
            classify_age(ago(7), n),
            AgeTier::Ageing,
            "7 days is the notice boundary"
        );
        assert_eq!(classify_age(ago(29), n), AgeTier::Ageing);
        assert_eq!(
            classify_age(ago(30), n),
            AgeTier::Stale,
            "30 days is the stale boundary"
        );
        assert_eq!(classify_age(ago(365), n), AgeTier::Stale);
    }

    #[test]
    fn age_in_days_is_whole_days_elapsed() {
        let n = now();
        assert_eq!(age_days(ago(14), n), 14);
        assert_eq!(age_days(n, n), 0);
    }

    #[test]
    fn an_item_just_under_a_boundary_does_not_cross_it() {
        // Guards the >= comparisons: 6 days 23 hours is still Fresh.
        let n = now();
        let almost = n - Duration::days(6) - Duration::hours(23);
        assert_eq!(classify_age(almost, n), AgeTier::Fresh);
    }
}
