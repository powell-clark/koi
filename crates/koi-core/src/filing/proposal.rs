//! Proposal — the unit of consent. Every user-file action flows through one.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Stable identifier for a proposal — hash of (monitor, path, action).
/// Used for idempotent storage: re-scanning won't create duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProposalId(pub String);

impl ProposalId {
    pub fn compute(monitor: &str, path: &std::path::Path, action: &ProposedAction) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        monitor.hash(&mut hasher);
        path.hash(&mut hasher);
        // Discriminate actions without full serde round-trip.
        match action {
            ProposedAction::Move { dest } => {
                "Move".hash(&mut hasher);
                dest.hash(&mut hasher);
            }
            ProposedAction::Archive { archive_root } => {
                "Archive".hash(&mut hasher);
                archive_root.hash(&mut hasher);
            }
            ProposedAction::Delete => {
                "Delete".hash(&mut hasher);
            }
            ProposedAction::Tag { labels } => {
                "Tag".hash(&mut hasher);
                for l in labels {
                    l.hash(&mut hasher);
                }
            }
            ProposedAction::Ignore { reason } => {
                "Ignore".hash(&mut hasher);
                reason.hash(&mut hasher);
            }
            ProposedAction::DriveMove {
                remote_src,
                remote_dest,
            } => {
                "DriveMove".hash(&mut hasher);
                remote_src.hash(&mut hasher);
                remote_dest.hash(&mut hasher);
            }
            ProposedAction::Review { summary } => {
                "Review".hash(&mut hasher);
                summary.hash(&mut hasher);
            }
        }
        ProposalId(format!("{:016x}", hasher.finish()))
    }
}

/// What the monitor wants to do with a file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposedAction {
    /// Move file to a specific destination.
    Move { dest: PathBuf },
    /// Move file into an archive root keyed by category (monitor decides structure).
    Archive { archive_root: PathBuf },
    /// Permanent delete — reserved for derivable / reproducible files only.
    Delete,
    /// Attach metadata (xattr labels). Non-destructive.
    Tag { labels: Vec<String> },
    /// Teach classifier "leave this kind of thing alone here".
    Ignore { reason: String },
    /// Move a file within a remote (rclone) filesystem.
    DriveMove {
        remote_src: String,
        remote_dest: String,
    },
    /// Flag a fact for a human to look at — no file operation of any kind.
    /// For discrepancies that are not about a file at all (ADR-0024: a
    /// cross-machine fleet-config mismatch), where the only "action" koi may
    /// ever take unattended is to say so.
    Review { summary: String },
}

/// How much authority koi needs to execute the action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyTier {
    /// Safe, reversible, system-level — koi may execute autonomously.
    Full,
    /// User files or config — require explicit consent.
    Approve,
    /// Destructive / irreversible — human-initiated only.
    Human,
}

/// A unit of consent. Immutable once emitted; state lives in the decisions table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: ProposalId,
    pub monitor: &'static str,
    pub path: PathBuf,
    pub action: ProposedAction,
    pub rationale: String,
    pub confidence: f32,
    pub autonomy_tier: AutonomyTier,
    pub emitted_at: DateTime<Utc>,
}

impl Proposal {
    pub fn new(
        monitor: &'static str,
        path: PathBuf,
        action: ProposedAction,
        rationale: impl Into<String>,
        confidence: f32,
    ) -> Self {
        let id = ProposalId::compute(monitor, &path, &action);
        let autonomy_tier = match action {
            ProposedAction::Delete | ProposedAction::Review { .. } => AutonomyTier::Human,
            ProposedAction::Ignore { .. } | ProposedAction::Tag { .. } => AutonomyTier::Full,
            ProposedAction::Move { .. }
            | ProposedAction::Archive { .. }
            | ProposedAction::DriveMove { .. } => AutonomyTier::Approve,
        };
        Self {
            id,
            monitor,
            path,
            action,
            rationale: rationale.into(),
            confidence: confidence.clamp(0.0, 1.0),
            autonomy_tier,
            emitted_at: Utc::now(),
        }
    }

    /// Raise this proposal to `AutonomyTier::Human`, marking it as something a
    /// person must look at individually rather than sweep in a batch.
    ///
    /// Monitors over content-bearing roots (Downloads, Documents, inbox, Drive)
    /// use this. Filing is extension-based and not content-aware, so a bank
    /// statement and a screenshot land in the same bucket and are
    /// indistinguishable to the batch path — on 2026-08-25 one `--all` call
    /// swept 334 personal documents in about 160ms. The tier is deliberately
    /// not part of the id hash, so raising it leaves existing rows addressable.
    pub fn requiring_human_review(mut self) -> Self {
        self.autonomy_tier = AutonomyTier::Human;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_id_is_stable() {
        let path = PathBuf::from("/home/user/Downloads/x.pdf");
        let act = ProposedAction::Move {
            dest: PathBuf::from("/home/user/Documents/x.pdf"),
        };
        let a = ProposalId::compute("Downloads", &path, &act);
        let b = ProposalId::compute("Downloads", &path, &act);
        assert_eq!(a, b);
    }

    #[test]
    fn delete_is_human_tier() {
        let p = Proposal::new(
            "M",
            PathBuf::from("/tmp/x"),
            ProposedAction::Delete,
            "r",
            0.9,
        );
        assert_eq!(p.autonomy_tier, AutonomyTier::Human);
    }

    #[test]
    fn move_is_approve_tier() {
        let p = Proposal::new(
            "M",
            PathBuf::from("/tmp/x"),
            ProposedAction::Move {
                dest: PathBuf::from("/y"),
            },
            "r",
            0.9,
        );
        assert_eq!(p.autonomy_tier, AutonomyTier::Approve);
    }

    #[test]
    fn confidence_clamped() {
        let p = Proposal::new("M", PathBuf::from("/x"), ProposedAction::Delete, "r", 2.5);
        assert!((p.confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn human_review_upgrades_the_tier_without_changing_the_id() {
        // The tier is not part of the id hash, so a monitor can raise it without
        // orphaning rows already in the proposals table (TASK-KOI229).
        let mk = || {
            Proposal::new(
                "DownloadsMonitor",
                PathBuf::from("/home/u/Downloads/statement.pdf"),
                ProposedAction::Move {
                    dest: PathBuf::from("/home/u/Documents/PDFs/statement.pdf"),
                },
                "PDF document",
                0.85,
            )
        };
        let plain = mk();
        let raised = mk().requiring_human_review();
        assert_eq!(plain.autonomy_tier, AutonomyTier::Approve);
        assert_eq!(raised.autonomy_tier, AutonomyTier::Human);
        assert_eq!(
            plain.id, raised.id,
            "raising the tier must not change the proposal identity"
        );
    }

    #[test]
    fn review_is_human_tier() {
        // ADR-0024: a fleet discrepancy is never auto-applied, so Review is
        // human-tier from construction rather than needing
        // requiring_human_review() to raise it there.
        let p = Proposal::new(
            "FleetConfigMonitor",
            PathBuf::from("/home/user/.bashrc"),
            ProposedAction::Review {
                summary: "shell-config differs from peer laptop".into(),
            },
            "r",
            0.9,
        );
        assert_eq!(p.autonomy_tier, AutonomyTier::Human);
    }

    #[test]
    fn requiring_human_review_never_downgrades_a_delete() {
        let p = Proposal::new("M", PathBuf::from("/x"), ProposedAction::Delete, "r", 0.5)
            .requiring_human_review();
        assert_eq!(p.autonomy_tier, AutonomyTier::Human);
    }
}
