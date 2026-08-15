//! InboxMonitor — scans `~/inbox/` for dropped files.
//!
//! `~/inbox/` is the unified drop-point described in ADR-0014. Mobile photos,
//! finance PDFs from a managed finance folder, scanned docs — anything whose
//! final home is unclear — lands here. InboxMonitor proposes a destination for
//! everything that gets classified, with a lower default confidence than DownloadsMonitor
//! so ambiguous items surface multiple candidate destinations (future UI).

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::{
    config::FilingConfig,
    filing::{FileMonitor, Proposal, ProposedAction, ScanContext},
    Result,
};

pub struct InboxMonitor {
    root: PathBuf,
    docs: PathBuf,
}

impl InboxMonitor {
    pub fn new() -> Result<Self> {
        let home = crate::state::home_dir()?;
        Ok(Self {
            root: home.join("inbox"),
            docs: home.join("Documents"),
        })
    }

    pub fn with_roots(inbox: PathBuf, documents: PathBuf) -> Self {
        Self {
            root: inbox,
            docs: documents,
        }
    }

    /// Like [`Self::new`], but a configured root override (if present) wins
    /// over the `$HOME`-derived default.
    pub fn from_config(cfg: &FilingConfig) -> Result<Self> {
        let home = crate::state::home_dir()?;
        Ok(Self {
            root: cfg
                .roots
                .inbox
                .clone()
                .unwrap_or_else(|| home.join("inbox")),
            docs: cfg
                .roots
                .documents
                .clone()
                .unwrap_or_else(|| home.join("Documents")),
        })
    }

    fn classify(&self, path: &Path, ctx: &ScanContext) -> Option<(PathBuf, &'static str, f32)> {
        let ext = path
            .extension()
            .and_then(OsStr::to_str)?
            .to_ascii_lowercase();
        let filename = path.file_name()?;
        let suffix = format!(".{ext}");

        if let Some(classifier) = &ctx.classifier {
            if let Some((learned_dir, conf)) = classifier.suggest("InboxMonitor", &suffix) {
                if conf >= 0.6 {
                    return Some((
                        learned_dir.join(filename),
                        "learned from inbox approvals",
                        conf,
                    ));
                }
            }
        }

        // Inbox confidences deliberately lower than Downloads: inbox items tend
        // to be more significant / ambiguous and we want to leave room for the
        // user to redirect.
        let (subdir, rationale, confidence) = match ext.as_str() {
            "pdf" => ("PDFs-Inbox", "PDF dropped to inbox", 0.70),
            "jpg" | "jpeg" | "png" | "heic" | "webp" | "gif" => {
                ("Photos-Inbox", "Photo dropped to inbox", 0.65)
            }
            "mp4" | "mov" | "mkv" | "webm" => ("Videos-Inbox", "Video dropped to inbox", 0.65),
            "docx" | "doc" | "odt" => ("Documents-Text", "Text document", 0.65),
            "xlsx" | "csv" => ("Spreadsheets", "Spreadsheet", 0.65),
            "md" | "txt" => ("Notes", "Note dropped to inbox", 0.65),
            _ => return None,
        };
        let dest = self.docs.join(subdir).join(filename);
        Some((dest, rationale, confidence))
    }
}

impl FileMonitor for InboxMonitor {
    fn name(&self) -> &'static str {
        "InboxMonitor"
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Proposal>> {
        if !self.root.exists() {
            return Ok(vec![]);
        }

        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(_) => return Ok(vec![]),
        };

        let mut proposals = Vec::new();
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            if ctx.is_managed(&path) {
                continue;
            }
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }
            if let Some((dest, rationale, confidence)) = self.classify(&path, ctx) {
                proposals.push(Proposal::new(
                    "InboxMonitor",
                    path,
                    ProposedAction::Move { dest },
                    rationale,
                    confidence,
                ));
            }
        }
        Ok(proposals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_config_applies_root_overrides() {
        let mut cfg = crate::config::FilingConfig::default();
        cfg.roots.inbox = Some(PathBuf::from("/mnt/scratch/inbox"));
        cfg.roots.documents = Some(PathBuf::from("/mnt/scratch/Documents"));
        let mon = InboxMonitor::from_config(&cfg).unwrap();
        assert_eq!(mon.roots(), vec![PathBuf::from("/mnt/scratch/inbox")]);
        assert_eq!(mon.docs, PathBuf::from("/mnt/scratch/Documents"));
    }
}
