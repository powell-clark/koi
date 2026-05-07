//! GitMonitor — uncommitted/unpushed state across a tree of repositories.
//!
//! The scan root is configurable via the `KOI_GIT_ROOT` environment variable
//! and defaults to `~/projects` when unset.
//!
//! Uses libgit2 via git2-rs (vendored, no system libgit2 dependency).

use chrono::Utc;
use git2::{BranchType, Repository, StatusOptions};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

use crate::{
    monitor::Monitor,
    types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion},
    Result,
};

const MAX_SCAN_DEPTH: usize = 3;

pub struct GitMonitor {
    scan_root: PathBuf,
}

impl GitMonitor {
    pub fn new() -> Result<Self> {
        // Explicit override wins; otherwise default to `~/projects`.
        if let Some(root) = std::env::var_os("KOI_GIT_ROOT") {
            return Ok(Self {
                scan_root: PathBuf::from(root),
            });
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| crate::error::Error::Config("$HOME not set".into()))?;
        Ok(Self {
            scan_root: home.join("projects"),
        })
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self { scan_root: root }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct RepoStatus {
    path: String,
    branch: String,
    uncommitted: usize,
    ahead: usize,
    behind: usize,
}

impl Monitor for GitMonitor {
    fn name(&self) -> &'static str {
        "GitMonitor"
    }

    fn run(&self) -> Result<MonitorReport> {
        let started = std::time::Instant::now();
        let repos = find_git_repos(&self.scan_root, MAX_SCAN_DEPTH);

        let statuses: Vec<RepoStatus> = repos.par_iter().filter_map(|p| inspect_repo(p)).collect();

        let dirty: Vec<&RepoStatus> = statuses.iter().filter(|s| s.uncommitted > 0).collect();
        let unpushed: Vec<&RepoStatus> = statuses.iter().filter(|s| s.ahead > 0).collect();
        let total_uncommitted: usize = dirty.iter().map(|s| s.uncommitted).sum();
        let total_ahead: usize = unpushed.iter().map(|s| s.ahead).sum();

        let status = if total_uncommitted > 50 || total_ahead > 10 {
            HealthStatus::Critical
        } else if !dirty.is_empty() || !unpushed.is_empty() {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };

        let observations = vec![
            Observation {
                key: "repos_scanned".into(),
                value: serde_json::json!(statuses.len()),
                severity: Severity::Info,
            },
            Observation {
                key: "dirty_repos".into(),
                value: serde_json::json!({
                    "count": dirty.len(),
                    "total_changes": total_uncommitted,
                    "sample": dirty.iter().take(5).collect::<Vec<_>>(),
                }),
                severity: if dirty.is_empty() {
                    Severity::Info
                } else {
                    Severity::Warning
                },
            },
            Observation {
                key: "unpushed_repos".into(),
                value: serde_json::json!({
                    "count": unpushed.len(),
                    "total_commits": total_ahead,
                    "sample": unpushed.iter().take(5).collect::<Vec<_>>(),
                }),
                severity: if unpushed.is_empty() {
                    Severity::Info
                } else {
                    Severity::Warning
                },
            },
        ];

        let mut suggestions = vec![];
        if !dirty.is_empty() {
            let names: Vec<&str> = dirty.iter().take(3).map(|s| s.path.as_str()).collect();
            suggestions.push(Suggestion {
                message: format!(
                    "{} repo(s) dirty ({} changes total): {}",
                    dirty.len(),
                    total_uncommitted,
                    names.join(", ")
                ),
                severity: Severity::Warning,
                action_hint: Some("Review and commit or stash".into()),
            });
        }
        if !unpushed.is_empty() {
            let names: Vec<&str> = unpushed.iter().take(3).map(|s| s.path.as_str()).collect();
            suggestions.push(Suggestion {
                message: format!(
                    "{} repo(s) ahead of remote ({} commits total): {}",
                    unpushed.len(),
                    total_ahead,
                    names.join(", ")
                ),
                severity: Severity::Warning,
                action_hint: Some("git push".into()),
            });
        }

        Ok(MonitorReport {
            monitor: self.name().into(),
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
            collected_at: Utc::now(),
            observations,
            suggestions,
        })
    }
}

fn find_git_repos(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    collect_repos(root, max_depth, &mut out);
    out
}

fn collect_repos(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if dir.join(".git").exists() {
        out.push(dir.to_path_buf());
        return; // don't recurse into a git repo
    }
    if depth == 0 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_dir() {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') || name == "node_modules" || name == "target" {
                    continue;
                }
            }
            collect_repos(&path, depth - 1, out);
        }
    }
}

fn inspect_repo(path: &Path) -> Option<RepoStatus> {
    let repo = Repository::open(path).ok()?;

    let mut opts = StatusOptions::new();
    opts.include_untracked(true).include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts)).ok()?;
    let uncommitted = statuses.len();

    let head = repo.head().ok()?;
    let branch_name = head.shorthand().unwrap_or("HEAD").to_string();

    let (ahead, behind) = upstream_delta(&repo, &branch_name).unwrap_or((0, 0));

    Some(RepoStatus {
        path: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string(),
        branch: branch_name,
        uncommitted,
        ahead,
        behind,
    })
}

fn upstream_delta(repo: &Repository, branch_name: &str) -> Option<(usize, usize)> {
    let branch = repo.find_branch(branch_name, BranchType::Local).ok()?;
    let upstream = branch.upstream().ok()?;
    let local_oid = branch.get().target()?;
    let upstream_oid = upstream.get().target()?;
    repo.graph_ahead_behind(local_oid, upstream_oid).ok()
}
