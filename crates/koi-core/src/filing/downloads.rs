//! DownloadsMonitor — first concrete FileMonitor.
//!
//! Scans `~/Downloads/` with seed classification rules keyed by extension.
//! Emits `ProposedAction::Move` for files matching a known category. Files
//! with unknown extensions or under managed zones are left alone.
//!
//! This is a v0 classifier: no learning yet. The executor + decisions table
//! (koi-core::state) accumulate training data; a future version queries them
//! instead of using this static table.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::{
    filing::{FileMonitor, Proposal, ProposedAction, ScanContext},
    Result,
};

const MAX_DEPTH: usize = 1; // Downloads root only — not subdirs

pub struct DownloadsMonitor {
    root: PathBuf,
    docs: PathBuf,
}

impl DownloadsMonitor {
    pub fn new() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| crate::error::Error::Config("$HOME not set".into()))?;
        Ok(Self {
            root: home.join("Downloads"),
            docs: home.join("Documents"),
        })
    }

    pub fn with_roots(downloads: PathBuf, documents: PathBuf) -> Self {
        Self {
            root: downloads,
            docs: documents,
        }
    }

    fn classify(&self, path: &Path, ctx: &ScanContext) -> Option<(PathBuf, &'static str, f32)> {
        let ext = path
            .extension()
            .and_then(OsStr::to_str)?
            .to_ascii_lowercase();
        let filename = path.file_name()?;
        let suffix = format!(".{ext}");

        // Learning-loop query: if the user has approved moves for this suffix
        // before, prefer that destination (higher confidence than seed rules).
        if let Some(classifier) = &ctx.classifier {
            if let Some((learned_dir, conf)) = classifier.suggest("DownloadsMonitor", &suffix) {
                if conf >= 0.6 {
                    return Some((learned_dir.join(filename), "learned from approvals", conf));
                }
            }
        }

        // Fall back to seed rules.
        let (subdir, rationale, confidence) = match ext.as_str() {
            "pdf" => ("PDFs", "PDF document", 0.85),
            "jpg" | "jpeg" | "png" | "heic" | "webp" | "gif" | "bmp" | "tiff" => {
                ("Images", "Image file", 0.80)
            }
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" => {
                ("Archives", "Archive", 0.75)
            }
            "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => ("Videos", "Video file", 0.80),
            "mp3" | "wav" | "m4a" | "flac" | "ogg" | "aac" => ("Audio", "Audio file", 0.80),
            "iso" | "img" => ("ISOs", "Disk image", 0.70),
            _ => return None,
        };
        let dest = self.docs.join(subdir).join(filename);
        Some((dest, rationale, confidence))
    }
}

impl FileMonitor for DownloadsMonitor {
    fn name(&self) -> &'static str {
        "DownloadsMonitor"
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Proposal>> {
        if !self.root.exists() {
            return Ok(vec![]);
        }

        let mut proposals = Vec::new();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(_) => return Ok(vec![]),
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            if ctx.is_managed(&path) {
                continue;
            }
            // Hidden files / dotfiles
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }

            if let Some((dest, rationale, confidence)) = self.classify(&path, ctx) {
                proposals.push(Proposal::new(
                    "DownloadsMonitor",
                    path,
                    ProposedAction::Move { dest },
                    rationale,
                    confidence,
                ));
            }
            // Unknown extensions: no proposal. DO NOT emit Ignore — that would
            // train the classifier away from learning them later. Silence is
            // just "no opinion".
        }

        let _ = MAX_DEPTH; // reserved for future recursion toggle
        Ok(proposals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(prefix: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("koi-dl-{prefix}-{nanos:x}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn scans_and_classifies() {
        let downloads = tmp("dl");
        let documents = tmp("docs");
        fs::write(downloads.join("report.pdf"), b"x").unwrap();
        fs::write(downloads.join("photo.jpg"), b"x").unwrap();
        fs::write(downloads.join("bundle.zip"), b"x").unwrap();
        fs::write(downloads.join("mystery.xyz"), b"x").unwrap(); // unknown ext
        fs::write(downloads.join(".hidden.pdf"), b"x").unwrap(); // hidden, skip

        let mon = DownloadsMonitor::with_roots(downloads.clone(), documents.clone());
        let ctx = ScanContext::new_now();
        let proposals = mon.scan(&ctx).unwrap();

        assert_eq!(
            proposals.len(),
            3,
            "expected pdf + jpg + zip, got {}",
            proposals.len()
        );
        let dests: Vec<_> = proposals
            .iter()
            .filter_map(|p| match &p.action {
                ProposedAction::Move { dest } => Some(dest.clone()),
                _ => None,
            })
            .collect();
        // ends_with matches whole path components, so this is separator-agnostic
        // (forward slash on Unix, backslash on Windows).
        use std::path::Path;
        assert!(dests
            .iter()
            .any(|d| d.ends_with(Path::new("PDFs").join("report.pdf"))));
        assert!(dests
            .iter()
            .any(|d| d.ends_with(Path::new("Images").join("photo.jpg"))));
        assert!(dests
            .iter()
            .any(|d| d.ends_with(Path::new("Archives").join("bundle.zip"))));

        fs::remove_dir_all(&downloads).ok();
        fs::remove_dir_all(&documents).ok();
    }

    #[test]
    fn empty_root_emits_nothing() {
        let downloads = tmp("empty-dl");
        let documents = tmp("empty-docs");
        let mon = DownloadsMonitor::with_roots(downloads.clone(), documents.clone());
        let ctx = ScanContext::new_now();
        let proposals = mon.scan(&ctx).unwrap();
        assert!(proposals.is_empty());
        fs::remove_dir_all(&downloads).ok();
        fs::remove_dir_all(&documents).ok();
    }
}
