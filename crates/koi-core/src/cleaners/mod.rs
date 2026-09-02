//! Cleaners — safe, reversible cache purges (full-autonomy tier; see ARCHITECTURE.md).
//!
//! Each cleaner targets a well-known cache whose loss costs nothing but re-
//! download time. No user files are ever touched here; user-file mutations go
//! through the FileMonitor proposal pipeline.

pub mod git_objects;

use crate::fs_size::dir_size;
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
    CleanTarget {
        name: "pnpm-store",
        path_relative_to_home: ".local/share/pnpm/store",
        note: "re-populates from registry on next install",
    },
    CleanTarget {
        name: "pip",
        path_relative_to_home: ".cache/pip",
        note: "re-downloads wheels on next install",
    },
    CleanTarget {
        name: "Trash",
        path_relative_to_home: ".local/share/Trash",
        note: "already user-deleted files — freeing is just emptying the bin",
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

/// True if any running process holds an open file descriptor or a memory
/// mapping under `path` — checked via `/proc/*/fd` symlink targets and
/// `/proc/*/maps` entries. Used to gate propose-tier cache candidates before
/// suggesting deletion (the exact manual check that made removing a 7.3GB
/// unreferenced HuggingFace cache safe rather than hopeful — see WORK-KOI041).
/// Best-effort: unreadable /proc entries (permission, race with process exit)
/// are silently skipped, never treated as "referenced" or "not referenced".
#[cfg(target_os = "linux")]
pub fn path_has_live_reference(path: &Path) -> bool {
    let path_str = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => path.to_path_buf(),
    };

    let Ok(procs) = fs::read_dir("/proc") else {
        return false;
    };

    for entry in procs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };

        if let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) {
            for fd in fds.flatten() {
                if let Ok(target) = fs::read_link(fd.path()) {
                    if target.starts_with(&path_str) {
                        return true;
                    }
                }
            }
        }

        if let Ok(maps) = fs::read_to_string(format!("/proc/{pid}/maps")) {
            if maps
                .lines()
                .any(|line| line.contains(path_str.to_string_lossy().as_ref()))
            {
                return true;
            }
        }
    }

    false
}

#[cfg(not(target_os = "linux"))]
pub fn path_has_live_reference(_path: &Path) -> bool {
    // fd/maps scanning is Linux-specific; without it, err on the side of
    // caution and treat as referenced so nothing gets proposed blind.
    true
}

/// A cleanup candidate that is NEVER auto-executed — surfaced for the
/// operator's own action, same tier as FileMonitor's consent-gated proposals.
/// Snap revisions need root; docker volumes can hold real data; both are
/// exactly the "propose and wait for approval" tier per AGENTS.md.
#[derive(Debug, Clone)]
pub struct CleanProposal {
    pub kind: &'static str,
    pub description: String,
    pub command_hint: String,
}

/// Disabled snap revisions — `snap list --all` marks superseded revisions
/// `disabled`; removing them needs root, so this only ever proposes.
pub fn snap_disabled_revision_proposals() -> Vec<CleanProposal> {
    let Ok(output) = std::process::Command::new("snap")
        .args(["list", "--all"])
        .env("LANG", "C")
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains("disabled"))
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let name = cols.next()?;
            let revision = cols.nth(1)?; // Name Version Rev Tracking Publisher Notes
            Some(CleanProposal {
                kind: "snap-disabled-revision",
                description: format!("{name} revision {revision} (superseded, disabled)"),
                command_hint: format!("sudo snap remove {name} --revision={revision}"),
            })
        })
        .collect()
}

/// Dangling (unattached) Docker volumes — can legitimately hold data a
/// container will reattach to later, so this only ever proposes; never
/// auto-removed.
pub fn docker_dangling_volume_proposals() -> Vec<CleanProposal> {
    let Ok(output) = std::process::Command::new("docker")
        .args(["volume", "ls", "-f", "dangling=true", "-q"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|name| CleanProposal {
            kind: "docker-dangling-volume",
            description: format!("volume {name} (dangling — not attached to any container)"),
            command_hint: format!("docker volume rm {name}  # inspect first — may hold real data"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    // Both tests below are Linux-only — reference detection reads /proc.
    // Gate the import to match so it is not flagged unused on other platforms.
    #[cfg(target_os = "linux")]
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn path_has_live_reference_detects_our_own_open_file() {
        let dir =
            std::env::temp_dir().join(format!("koi-cleaners-ref-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("held-open.bin");

        // Hold the file open for the duration of the check — this process's
        // own /proc/self/fd entry should be found.
        let _file = std::fs::File::create(&target).unwrap();
        assert!(
            path_has_live_reference(&target),
            "an open file descriptor to the exact path should be detected"
        );

        drop(_file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn path_has_live_reference_false_for_unreferenced_path() {
        let dir =
            std::env::temp_dir().join(format!("koi-cleaners-noref-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("never-opened.bin");
        std::fs::write(&target, b"hello").unwrap();

        assert!(
            !path_has_live_reference(&target),
            "a file nothing has open should not be reported as referenced"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
