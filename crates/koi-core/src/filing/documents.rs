//! DocumentsMonitor — scans `~/Documents/` root for loose files.
//!
//! Unlike DownloadsMonitor, which moves files OUT of Downloads entirely,
//! DocumentsMonitor moves files DOWN into organised subdirectories. Managed
//! zones (`.koi-managed-by`) in child directories are honoured — e.g. a
//! Finance/ folder owned by an external finance system gets skipped entirely.
//!
//! Depth: root only (no recursion). Subdirectory contents are assumed to be
//! already-filed; koi doesn't re-file them.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::{
    filing::{FileMonitor, Proposal, ProposedAction, ScanContext},
    Result,
};

pub struct DocumentsMonitor {
    root: PathBuf,
}

impl DocumentsMonitor {
    pub fn new() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| crate::error::Error::Config("$HOME not set".into()))?;
        Ok(Self {
            root: home.join("Documents"),
        })
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    fn classify(&self, path: &Path, ctx: &ScanContext) -> Option<(PathBuf, &'static str, f32)> {
        let ext = path
            .extension()
            .and_then(OsStr::to_str)?
            .to_ascii_lowercase();
        let filename = path.file_name()?;
        let suffix = format!(".{ext}");

        if let Some(classifier) = &ctx.classifier {
            if let Some((learned_dir, conf)) = classifier.suggest("DocumentsMonitor", &suffix) {
                if conf >= 0.6 {
                    return Some((learned_dir.join(filename), "learned from approvals", conf));
                }
            }
        }

        let (subdir, rationale, confidence) = match ext.as_str() {
            "pdf" => ("PDFs", "PDF in Documents root", 0.80),
            "jpg" | "jpeg" | "png" | "heic" | "webp" | "gif" => {
                ("Images", "Image in Documents root", 0.75)
            }
            "docx" | "doc" | "odt" | "rtf" => ("Documents-Text", "Text document", 0.75),
            "xlsx" | "xls" | "ods" | "csv" | "tsv" => ("Spreadsheets", "Spreadsheet", 0.75),
            "pptx" | "ppt" | "odp" => ("Presentations", "Presentation", 0.75),
            "md" | "txt" => ("Notes", "Plain text note", 0.70),
            _ => return None,
        };
        let dest = self.root.join(subdir).join(filename);
        Some((dest, rationale, confidence))
    }
}

impl FileMonitor for DocumentsMonitor {
    fn name(&self) -> &'static str {
        "DocumentsMonitor"
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
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }
            if let Some((dest, rationale, confidence)) = self.classify(&path, ctx) {
                proposals.push(Proposal::new(
                    "DocumentsMonitor",
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
    use std::{fs, io::Write};

    fn tmp(prefix: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("koi-docs-{prefix}-{nanos:x}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn classifies_documents_root_files() {
        let docs = tmp("root");
        fs::write(docs.join("notes.md"), b"x").unwrap();
        fs::write(docs.join("budget.xlsx"), b"x").unwrap();
        fs::write(docs.join("x.unknown"), b"x").unwrap();

        let mon = DocumentsMonitor::with_root(docs.clone());
        let ctx = ScanContext::new_now();
        let proposals = mon.scan(&ctx).unwrap();
        assert_eq!(proposals.len(), 2, "md + xlsx classified, unknown skipped");
        fs::remove_dir_all(&docs).ok();
    }

    #[test]
    fn honours_managed_zone_marker() {
        let docs = tmp("mz");
        fs::create_dir_all(docs.join("Finance")).unwrap();
        let mut m = fs::File::create(docs.join("Finance/.koi-managed-by")).unwrap();
        writeln!(m, "system = \"the-book\"\nscope = \"recursive\"").unwrap();
        // Put a loose file in Documents root AND one inside Finance.
        fs::write(docs.join("loose.pdf"), b"x").unwrap();
        fs::write(docs.join("Finance/statement.pdf"), b"x").unwrap();

        let mon = DocumentsMonitor::with_root(docs.clone());
        let ctx = ScanContext::new_now_with_roots(std::slice::from_ref(&docs));
        let proposals = mon.scan(&ctx).unwrap();
        // Only loose.pdf from root (Finance not scanned — we're depth 1).
        // More importantly: even if we tried, Finance would be a managed zone.
        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].path.ends_with("loose.pdf"));
        fs::remove_dir_all(&docs).ok();
    }

    #[test]
    fn honours_root_itself_being_managed_via_context() {
        // The whole Documents root claimed by another system — a case the old
        // per-scan local ZoneCache (which only ever registered *children* of
        // root, never root itself) could not represent at all. Only reachable
        // now that the shared ScanContext is populated by ZoneCache::discover,
        // which registers the root passed in, not just its children.
        let docs = tmp("root-managed");
        let mut m = fs::File::create(docs.join(".koi-managed-by")).unwrap();
        writeln!(m, "system = \"the-book\"\nscope = \"recursive\"").unwrap();
        fs::write(docs.join("loose.pdf"), b"x").unwrap();

        let mon = DocumentsMonitor::with_root(docs.clone());
        let ctx = ScanContext::new_now_with_roots(std::slice::from_ref(&docs));
        let proposals = mon.scan(&ctx).unwrap();
        assert_eq!(
            proposals.len(),
            0,
            "root-level managed zone must suppress every loose file under it"
        );
        fs::remove_dir_all(&docs).ok();
    }
}
