//! RootClutterMonitor — scans `$HOME` root-level and `$HOME/Desktop` for
//! tmp/backup-pattern files, loose non-dot files, and broken symlinks.
//! FEAT-KOI055 AC-3.
//!
//! Depth: root only (no recursion), matching the other local monitors.
//! Ordinary dotfiles (config) are never touched — the monitor targets
//! clutter, not configuration. Tmp/backup-pattern files propose a Move into
//! the same reversible trash `koi dedupe apply` uses (TASK-KOI210); loose
//! non-dot files propose a Move into `~/inbox/`; broken symlinks propose an
//! informational `Ignore` and are never touched.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::{
    config::FilingConfig,
    filing::{FileMonitor, Proposal, ProposedAction, ScanContext},
    trash, Result,
};

pub struct RootClutterMonitor {
    roots: Vec<PathBuf>,
    trash_root: PathBuf,
    home: PathBuf,
    inbox: PathBuf,
}

impl RootClutterMonitor {
    pub fn new() -> Result<Self> {
        let home = crate::state::home_dir()?;
        let trash_root = trash::default_trash_root()?;
        Ok(Self {
            roots: vec![home.clone(), home.join("Desktop")],
            inbox: home.join("inbox"),
            trash_root,
            home,
        })
    }

    /// Like [`Self::new`], but a configured inbox root override (if present)
    /// wins over the `$HOME`-derived default — matching the other monitors'
    /// `from_config` convention. `roots`/`trash_root` are not yet
    /// configurable (no FilingConfig field for them exists today).
    pub fn from_config(cfg: &FilingConfig) -> Result<Self> {
        let home = crate::state::home_dir()?;
        let trash_root = trash::default_trash_root()?;
        let inbox = cfg
            .roots
            .inbox
            .clone()
            .unwrap_or_else(|| home.join("inbox"));
        Ok(Self {
            roots: vec![home.clone(), home.join("Desktop")],
            inbox,
            trash_root,
            home,
        })
    }

    pub fn with_roots(
        roots: Vec<PathBuf>,
        trash_root: PathBuf,
        home: PathBuf,
        inbox: PathBuf,
    ) -> Self {
        Self {
            roots,
            trash_root,
            home,
            inbox,
        }
    }

    fn is_tmp_or_bak_pattern(filename: &str) -> bool {
        let lower = filename.to_ascii_lowercase();
        lower.contains(".tmp") || lower.contains(".bak")
    }

    fn classify(&self, path: &Path, ctx: &ScanContext) -> Option<Proposal> {
        if path.is_symlink() && !path.exists() {
            return Some(Proposal::new(
                "RootClutterMonitor",
                path.to_path_buf(),
                ProposedAction::Ignore {
                    reason: "broken symlink — target does not exist".into(),
                },
                "dangling symlink",
                1.0,
            ));
        }

        let meta = std::fs::symlink_metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let filename = path.file_name().and_then(OsStr::to_str)?;

        if Self::is_tmp_or_bak_pattern(filename) {
            let dest = trash::trash_destination(path, &self.trash_root, &self.home, ctx.now);
            return Some(Proposal::new(
                "RootClutterMonitor",
                path.to_path_buf(),
                ProposedAction::Move { dest },
                "tmp/backup-pattern file at $HOME or Desktop root",
                0.85,
            ));
        }

        if filename.starts_with('.') {
            return None; // ordinary dotfile — leave config alone
        }

        let dest = self.inbox.join(filename);
        Some(Proposal::new(
            "RootClutterMonitor",
            path.to_path_buf(),
            ProposedAction::Move { dest },
            "loose file at $HOME or Desktop root",
            0.6,
        ))
    }
}

impl FileMonitor for RootClutterMonitor {
    fn name(&self) -> &'static str {
        "RootClutterMonitor"
    }

    fn roots(&self) -> Vec<PathBuf> {
        self.roots.clone()
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Proposal>> {
        let mut proposals = Vec::new();
        for root in &self.roots {
            if !root.exists() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if ctx.is_managed(&path) {
                    continue;
                }
                // Directories (other than broken symlinks) are out of
                // scope — this monitor is depth-1, files and dangling
                // symlinks only.
                if path.is_dir() {
                    continue;
                }
                if let Some(p) = self.classify(&path, ctx) {
                    proposals.push(p);
                }
            }
        }
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
        let p = std::env::temp_dir().join(format!("koi-rootclutter-{prefix}-{nanos:x}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn monitor_for(home: &Path) -> RootClutterMonitor {
        RootClutterMonitor::with_roots(
            vec![home.to_path_buf()],
            home.join("trash"),
            home.to_path_buf(),
            home.join("inbox"),
        )
    }

    #[test]
    fn tmp_pattern_file_proposes_move_to_trash() {
        let home = tmp("tmp-pattern");
        fs::write(home.join(".claude.json.tmp.12345.abcde"), b"x").unwrap();

        let mon = monitor_for(&home);
        let ctx = ScanContext::new_now();
        let proposals = mon.scan(&ctx).unwrap();

        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].path.ends_with(".claude.json.tmp.12345.abcde"));
        match &proposals[0].action {
            ProposedAction::Move { dest } => {
                assert!(dest.starts_with(home.join("trash")));
            }
            other => panic!("expected Move, got {other:?}"),
        }
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn bak_pattern_file_proposes_move_to_trash() {
        let home = tmp("bak-pattern");
        fs::write(home.join(".bashrc.bak-20260101"), b"x").unwrap();

        let mon = monitor_for(&home);
        let ctx = ScanContext::new_now();
        let proposals = mon.scan(&ctx).unwrap();

        assert_eq!(proposals.len(), 1);
        assert!(matches!(proposals[0].action, ProposedAction::Move { .. }));
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn loose_non_dot_file_proposes_move_to_inbox() {
        let home = tmp("loose");
        fs::write(home.join("lynis-report.dat"), b"x").unwrap();

        let mon = monitor_for(&home);
        let ctx = ScanContext::new_now();
        let proposals = mon.scan(&ctx).unwrap();

        assert_eq!(proposals.len(), 1);
        match &proposals[0].action {
            ProposedAction::Move { dest } => {
                assert_eq!(dest, &home.join("inbox").join("lynis-report.dat"));
            }
            other => panic!("expected Move, got {other:?}"),
        }
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn tmp_residue_stays_batch_sweepable() {
        // The counterpart to the Downloads tier test: RootClutterMonitor sweeps
        // machine residue, not the operator's documents, so `koi approve --all`
        // must keep working on it (TASK-KOI229).
        let home = tmp("residue-tier");
        fs::write(home.join(".claude.json.tmp.4242.abcdef"), b"x").unwrap();

        let mon = monitor_for(&home);
        let proposals = mon.scan(&ScanContext::new_now()).unwrap();

        assert_eq!(proposals.len(), 1);
        assert_eq!(
            proposals[0].autonomy_tier,
            crate::filing::AutonomyTier::Approve,
            "residue is not content-bearing; holding it back would starve the loop"
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn ordinary_dotfile_is_never_proposed() {
        let home = tmp("ordinary-dotfile");
        fs::write(home.join(".bashrc"), b"export PATH=x").unwrap();
        fs::write(home.join(".zshrc"), b"export PATH=x").unwrap();

        let mon = monitor_for(&home);
        let ctx = ScanContext::new_now();
        let proposals = mon.scan(&ctx).unwrap();

        assert!(
            proposals.is_empty(),
            "ordinary config dotfiles must never be proposed, got {proposals:?}"
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    #[cfg(unix)]
    fn broken_symlink_proposes_ignore_and_is_never_touched() {
        let home = tmp("broken-symlink");
        let target = home.join("gone-target");
        let link = home.join("dangling-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!target.exists(), "fixture assumption: target absent");

        let mon = monitor_for(&home);
        let ctx = ScanContext::new_now();
        let proposals = mon.scan(&ctx).unwrap();

        assert_eq!(proposals.len(), 1);
        assert!(matches!(proposals[0].action, ProposedAction::Ignore { .. }));
        assert!(
            link.is_symlink(),
            "an Ignore proposal must never remove the symlink"
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn honours_managed_zone_marker() {
        let home = tmp("managed");
        fs::write(
            home.join(".koi-managed-by"),
            "system = \"other\"\nscope = \"recursive\"",
        )
        .unwrap();
        fs::write(home.join("loose.txt"), b"x").unwrap();

        let mon = monitor_for(&home);
        let ctx = ScanContext::new_now_with_roots(std::slice::from_ref(&home));
        let proposals = mon.scan(&ctx).unwrap();

        assert!(
            proposals.is_empty(),
            "a root claimed by another system must not be proposed against"
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn directories_are_never_proposed() {
        let home = tmp("dirs");
        fs::create_dir_all(home.join("some-subdir")).unwrap();

        let mon = monitor_for(&home);
        let ctx = ScanContext::new_now();
        let proposals = mon.scan(&ctx).unwrap();

        assert!(proposals.is_empty());
        fs::remove_dir_all(&home).ok();
    }
}
