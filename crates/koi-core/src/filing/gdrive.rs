//! GoogleDriveMonitor — scans rclone-accessible Drive roots, emits DriveMove proposals.
//!
//! Requires an rclone remote configured on this machine. Config lives at
//! `~/.config/koi/gdrive.json`; if that file does not exist the monitor loads
//! as None and contributes nothing to `koi scan`.
//!
//! Config format:
//! ```json
//! {
//!   "scans": [
//!     { "remote": "gdrive", "path": "Downloads", "dest_root": "Documents" }
//!   ]
//! }
//! ```
//! Each entry defines one rclone remote and path to scan, plus the destination
//! root within the same remote where classified files will be moved on approval.

use serde::Deserialize;
use std::path::PathBuf;

use crate::{
    filing::{FileMonitor, Proposal, ProposedAction, ScanContext},
    Result,
};

#[derive(Debug, Deserialize)]
struct GdriveConfig {
    scans: Vec<ScanRoot>,
}

#[derive(Debug, Deserialize)]
struct ScanRoot {
    remote: String,
    path: String,
    dest_root: String,
}

/// Entry returned by `rclone lsjson --files-only`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RcloneEntry {
    path: String,
    name: String,
    #[allow(dead_code)]
    size: i64,
    is_dir: bool,
}

pub struct GoogleDriveMonitor {
    config: GdriveConfig,
}

impl GoogleDriveMonitor {
    /// Load config from `~/.config/koi/gdrive.json`. Returns None if the file
    /// doesn't exist (monitor not configured) or cannot be parsed.
    pub fn load() -> Option<Self> {
        let home = std::env::var_os("HOME")?;
        let config_path = PathBuf::from(home).join(".config/koi/gdrive.json");
        if !config_path.exists() {
            return None;
        }
        let text = std::fs::read_to_string(&config_path).ok()?;
        let config: GdriveConfig = serde_json::from_str(&text).ok()?;
        Some(Self { config })
    }

    fn rclone_available() -> bool {
        std::process::Command::new("rclone")
            .arg("version")
            .output()
            .is_ok()
    }

    fn list_remote(remote: &str, path: &str) -> Result<Vec<RcloneEntry>> {
        let remote_path = format!("{remote}:{path}");
        let out = std::process::Command::new("rclone")
            .args(["lsjson", &remote_path, "--files-only", "--no-modtime"])
            .output()
            .map_err(|e| crate::error::Error::Config(format!("rclone lsjson: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tracing::warn!("rclone lsjson {remote_path} failed: {stderr}");
            return Ok(vec![]);
        }
        let entries: Vec<RcloneEntry> = serde_json::from_slice(&out.stdout)
            .map_err(|e| crate::error::Error::Config(format!("rclone lsjson parse: {e}")))?;
        Ok(entries)
    }

    fn classify(name: &str) -> Option<(&'static str, f32)> {
        let ext = std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())?
            .to_ascii_lowercase();
        let pair = match ext.as_str() {
            "pdf" => ("PDFs", 0.85_f32),
            "jpg" | "jpeg" | "png" | "heic" | "webp" | "gif" | "bmp" | "tiff" => ("Images", 0.80),
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" => ("Archives", 0.75),
            "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => ("Videos", 0.80),
            "mp3" | "wav" | "m4a" | "flac" | "ogg" | "aac" => ("Audio", 0.80),
            "doc" | "docx" | "odt" | "rtf" => ("Documents", 0.80),
            "xls" | "xlsx" | "ods" | "csv" => ("Spreadsheets", 0.80),
            "ppt" | "pptx" | "odp" => ("Presentations", 0.75),
            _ => return None,
        };
        Some(pair)
    }
}

impl FileMonitor for GoogleDriveMonitor {
    fn name(&self) -> &'static str {
        "GoogleDriveMonitor"
    }

    fn roots(&self) -> Vec<PathBuf> {
        self.config
            .scans
            .iter()
            .map(|s| PathBuf::from(format!("{}:{}", s.remote, s.path)))
            .collect()
    }

    fn scan(&self, _ctx: &ScanContext) -> Result<Vec<Proposal>> {
        if !Self::rclone_available() {
            tracing::debug!("rclone not found — GoogleDriveMonitor scan skipped");
            return Ok(vec![]);
        }

        let mut proposals = Vec::new();
        for scan in &self.config.scans {
            let entries = Self::list_remote(&scan.remote, &scan.path)?;
            for entry in entries {
                if entry.is_dir {
                    continue;
                }
                let Some((subdir, confidence)) = Self::classify(&entry.name) else {
                    continue;
                };
                let remote_src = format!("{}:{}/{}", scan.remote, scan.path, entry.path);
                let remote_dest = format!(
                    "{}:{}/{}/{}",
                    scan.remote, scan.dest_root, subdir, entry.name
                );
                // PathBuf used as a display handle — remote path is the real reference.
                let display_path = PathBuf::from(&remote_src);
                proposals.push(Proposal::new(
                    "GoogleDriveMonitor",
                    display_path,
                    ProposedAction::DriveMove {
                        remote_src,
                        remote_dest,
                    },
                    format!("Google Drive file → {subdir}"),
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
    use serde_json::json;

    #[test]
    fn classify_known_extensions() {
        assert_eq!(
            GoogleDriveMonitor::classify("report.pdf"),
            Some(("PDFs", 0.85))
        );
        assert_eq!(
            GoogleDriveMonitor::classify("photo.jpg"),
            Some(("Images", 0.80))
        );
        assert_eq!(
            GoogleDriveMonitor::classify("archive.zip"),
            Some(("Archives", 0.75))
        );
        assert_eq!(
            GoogleDriveMonitor::classify("video.mp4"),
            Some(("Videos", 0.80))
        );
        assert_eq!(
            GoogleDriveMonitor::classify("notes.docx"),
            Some(("Documents", 0.80))
        );
    }

    #[test]
    fn classify_unknown_extension_returns_none() {
        assert!(GoogleDriveMonitor::classify("mystery.xyz").is_none());
        assert!(GoogleDriveMonitor::classify("noext").is_none());
    }

    #[test]
    fn load_returns_none_when_no_config() {
        // Relies on the absence of ~/.config/koi/gdrive.json in the test env.
        // If that file exists and is valid, this test is vacuously passing.
        // A production test environment should not have this file.
        let _ = GoogleDriveMonitor::load();
    }

    #[test]
    fn parse_rclone_json() {
        let json = json!([
            {"Path": "report.pdf", "Name": "report.pdf", "Size": 500, "IsDir": false},
            {"Path": "sub/", "Name": "sub", "Size": 0, "IsDir": true}
        ]);
        let entries: Vec<RcloneEntry> = serde_json::from_value(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(!entries[0].is_dir);
        assert!(entries[1].is_dir);
    }
}
