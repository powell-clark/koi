//! Cleaners — safe, reversible cache purges (full-autonomy tier; see ARCHITECTURE.md).
//!
//! Each cleaner targets a well-known cache whose loss costs nothing but re-
//! download time. No user files are ever touched here; user-file mutations go
//! through the FileMonitor proposal pipeline.

use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct CleanTarget {
    pub name: &'static str,
    pub path_relative_to_home: &'static str,
    pub note: &'static str,
}

/// Well-known caches that are safe to clean; each re-populates on next use.
pub const SAFE_CACHES: &[CleanTarget] = &[
    CleanTarget {
        name: "Playwright",
        path_relative_to_home: ".cache/ms-playwright",
        note: "re-downloads on next use",
    },
    CleanTarget {
        name: "pre-commit",
        path_relative_to_home: ".cache/pre-commit",
        note: "re-installs hooks on next commit",
    },
    CleanTarget {
        name: "TypeScript",
        path_relative_to_home: ".cache/typescript",
        note: "re-built on first compile",
    },
    CleanTarget {
        name: "puppeteer",
        path_relative_to_home: ".cache/puppeteer",
        note: "re-downloads browser on next use",
    },
    CleanTarget {
        name: "chrome-devtools-mcp",
        path_relative_to_home: ".cache/chrome-devtools-mcp",
        note: "re-fetched on next MCP session",
    },
    CleanTarget {
        name: "tracker3",
        path_relative_to_home: ".cache/tracker3",
        note: "GNOME file indexer rebuilds",
    },
    CleanTarget {
        name: "Cypress-cache",
        path_relative_to_home: ".cache/Cypress",
        note: "re-downloads binaries",
    },
    CleanTarget {
        name: "Cypress-config",
        path_relative_to_home: ".config/Cypress",
        note: "only if Cypress caches cleared",
    },
];

#[derive(Debug, Clone)]
pub struct CleanResult {
    pub target: &'static str,
    pub path: PathBuf,
    pub existed: bool,
    pub freed_bytes: u64,
    pub error: Option<String>,
}

pub fn plan(home: &Path) -> Vec<(CleanTarget, PathBuf, bool, u64)> {
    SAFE_CACHES
        .iter()
        .map(|t| {
            let path = home.join(t.path_relative_to_home);
            let existed = path.exists();
            let size = if existed { dir_size(&path) } else { 0 };
            (
                CleanTarget {
                    name: t.name,
                    path_relative_to_home: t.path_relative_to_home,
                    note: t.note,
                },
                path,
                existed,
                size,
            )
        })
        .collect()
}

pub fn execute_target(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    fs::remove_dir_all(path)
}

fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}
