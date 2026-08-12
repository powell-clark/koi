//! File lifecycle management — consent-gated filing, managed zones, learning.
//!
//! Defined by ADR-0014. Scanning is read-only; mutations happen only after an
//! approval is applied via [`Proposal::apply`]. Every user-file action flows
//! through a [`Proposal`] so the consent log is complete and durable.

pub mod classifier;
pub mod documents;
pub mod downloads;
pub mod executor;
pub mod gdrive;
pub mod inbox;
pub mod managed_zone;
pub mod proposal;

pub use classifier::{Classifier, SqliteClassifier};
pub use documents::DocumentsMonitor;
pub use downloads::DownloadsMonitor;
pub use executor::{apply, apply_or_err, Outcome};
pub use gdrive::GoogleDriveMonitor;
pub use inbox::InboxMonitor;
pub use managed_zone::{load_zone, ManagedZone};
pub use proposal::{AutonomyTier, Proposal, ProposalId, ProposedAction};

use crate::Result;
use std::{path::PathBuf, time::Duration};

/// Read-only scan contract. Implementations emit proposals; they never mutate.
pub trait FileMonitor: Send + Sync {
    fn name(&self) -> &'static str;

    /// Directory roots this monitor scans. Subdirectories claimed by other
    /// systems (via `.koi-managed-by` markers) are skipped by the scan helper.
    fn roots(&self) -> Vec<PathBuf>;

    /// Walk the roots, emit proposals. Pure; no I/O beyond reads.
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Proposal>>;

    /// Daemon hint — how often to re-scan. Defaults to daily.
    fn cadence(&self) -> Duration {
        Duration::from_secs(86_400)
    }
}

/// Context passed to a scan — carries clock, classifier, and zone cache so the
/// trait stays testable without real I/O.
pub struct ScanContext {
    pub now: chrono::DateTime<chrono::Utc>,
    pub zone_cache: managed_zone::ZoneCache,
    pub classifier: Option<Box<dyn Classifier>>,
}

impl std::fmt::Debug for ScanContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanContext")
            .field("now", &self.now)
            .field("zones", &self.zone_cache.zones().count())
            .field("has_classifier", &self.classifier.is_some())
            .finish()
    }
}

impl ScanContext {
    pub fn new_now() -> Self {
        Self {
            now: chrono::Utc::now(),
            zone_cache: managed_zone::ZoneCache::default(),
            classifier: None,
        }
    }

    /// Like [`ScanContext::new_now`], but the zone cache is populated by
    /// discovering `.koi-managed-by` markers under `roots` (and their direct
    /// children) up front — so every monitor sharing this context sees the
    /// same managed zones, not just the one monitor that happened to walk
    /// into a marked directory. Pass the union of every monitor's
    /// [`FileMonitor::roots`] for the scan session this context serves.
    pub fn new_now_with_roots(roots: &[PathBuf]) -> Self {
        Self {
            now: chrono::Utc::now(),
            zone_cache: managed_zone::ZoneCache::discover(roots),
            classifier: None,
        }
    }

    pub fn with_classifier(mut self, c: Box<dyn Classifier>) -> Self {
        self.classifier = Some(c);
        self
    }

    /// Returns true if `path` is under a managed zone and should be skipped.
    pub fn is_managed(&self, path: &std::path::Path) -> bool {
        self.zone_cache.is_managed(path)
    }
}

#[cfg(test)]
mod scan_context_tests {
    use super::*;

    #[test]
    fn new_now_with_roots_populates_zone_cache() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!(
            "koi-scanctx-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut f = std::fs::File::create(root.join(managed_zone::MARKER_FILENAME)).unwrap();
        writeln!(f, "system = \"the-book\"\nscope = \"recursive\"").unwrap();

        let ctx = ScanContext::new_now_with_roots(std::slice::from_ref(&root));
        assert!(ctx.is_managed(&root.join("x.pdf")));

        // Sanity: the plain constructor stays empty — no surprise behaviour
        // change for the many existing tests that call it directly.
        let empty_ctx = ScanContext::new_now();
        assert!(!empty_ctx.is_managed(&root.join("x.pdf")));

        std::fs::remove_dir_all(&root).ok();
    }
}
