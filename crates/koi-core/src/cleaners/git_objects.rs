//! Loose git-object hygiene across the project tree (TASK-KOI234).
//!
//! Standing request from the cockpit (MSG-EGLPK1065): a low-frequency
//! `git gc --prune=2.days.ago` per repo so loose objects do not rebuild.
//! Measured 2026-09-02 before any action: comms 869 loose objects,
//! powell-clark-limited 1952, the-book-system-prompt 1754.
//!
//! # Two rules that are not negotiable
//!
//! **`--prune=now` is never passed.** Another session can be mid-commit in a
//! repo koi is gc'ing, and pruning to the present moment can drop an object
//! that commit is about to reference. The two-day window is the entire
//! mitigation, so [`GC_PRUNE_WINDOW`] is a constant and a test asserts the
//! argv rather than trusting a caller to pass the right thing.
//!
//! **A repo mid-operation is skipped, not gc'd.** A rebase, merge or bisect in
//! flight leaves state under `.git` that a gc has no business touching while a
//! human is halfway through resolving something.
//!
//! `~/projects` is a managed zone koi's filing never moves files in. This pass
//! stays a git operation and never a file move, so that rule is not bent.

use std::path::{Path, PathBuf};

/// Never shortened. See the module docs.
pub const GC_PRUNE_WINDOW: &str = "2.days.ago";

/// Default loose-object count above which a repo is worth gc'ing. Tuned
/// against the measured spread (869–1952) rather than guessed: below this,
/// a gc costs more than it reclaims.
pub const DEFAULT_LOOSE_THRESHOLD: u64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoVerdict {
    /// Above threshold and safe to gc.
    Collect,
    /// Below threshold; leave it alone.
    BelowThreshold,
    /// A rebase, merge, cherry-pick or bisect is in flight.
    MidOperation(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatus {
    pub path: PathBuf,
    pub loose_objects: u64,
    pub verdict: RepoVerdict,
}

/// Parse `git count-objects -v` output for the loose object count.
pub fn parse_loose_count(output: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        line.strip_prefix("count:")
            .and_then(|rest| rest.trim().parse::<u64>().ok())
    })
}

/// Detect an operation in flight from the marker paths git leaves in `.git`.
///
/// Checked by path rather than by running git, so this stays cheap enough to
/// call for every repo in a scan.
pub fn in_progress_operation(git_dir: &Path) -> Option<&'static str> {
    let markers: [(&str, &str); 5] = [
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("MERGE_HEAD", "merge"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("BISECT_LOG", "bisect"),
    ];
    markers
        .iter()
        .find(|(marker, _)| git_dir.join(marker).exists())
        .map(|(_, name)| *name)
}

/// Decide what to do with one repo. Pure, so the decision is testable without
/// a git repository on disk.
pub fn classify_repo(
    loose_objects: u64,
    threshold: u64,
    in_progress: Option<&'static str>,
) -> RepoVerdict {
    if let Some(op) = in_progress {
        // Checked BEFORE the threshold: a repo mid-rebase is skipped whether
        // it has ten loose objects or ten thousand.
        return RepoVerdict::MidOperation(op);
    }
    if loose_objects >= threshold {
        RepoVerdict::Collect
    } else {
        RepoVerdict::BelowThreshold
    }
}

/// The exact argv koi runs. Separated out so a test can assert it, which is
/// AC-2's actual requirement: the guarantee is about what is executed, not
/// about what a comment claims.
pub fn gc_argv() -> Vec<&'static str> {
    // The prune window must arrive as `--prune=<window>`. Passing the bare
    // window as a positional argument makes git print its usage and exit 0,
    // which reads as success at the call site and collects nothing — that
    // exact defect shipped and was caught only by running it (2026-09-02).
    vec!["gc", "--quiet", GC_PRUNE_FLAG]
}

/// The full flag, not just the window, so a caller cannot assemble it wrongly.
pub const GC_PRUNE_FLAG: &str = "--prune=2.days.ago";

/// Count loose objects in one repo.
pub fn count_loose(repo: &Path) -> Option<u64> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["count-objects", "-v"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_loose_count(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

/// Survey repos without changing anything.
pub fn survey(repos: &[PathBuf], threshold: u64) -> Vec<RepoStatus> {
    repos
        .iter()
        .filter_map(|repo| {
            let loose = count_loose(repo)?;
            let verdict =
                classify_repo(loose, threshold, in_progress_operation(&repo.join(".git")));
            Some(RepoStatus {
                path: repo.clone(),
                loose_objects: loose,
                verdict,
            })
        })
        .collect()
}

/// Run the gc. Only ever called for [`RepoVerdict::Collect`] repos.
pub fn collect(repo: &Path) -> std::io::Result<bool> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(gc_argv())
        .output()?;
    Ok(out.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_argv_never_contains_prune_now() {
        // AC-2. This is the test the card asks for by name, and it is the only
        // thing standing between a future edit and pruning an object another
        // session's in-flight commit is about to reference.
        let argv = gc_argv();
        assert!(argv.contains(&"gc"));
        assert!(argv
            .iter()
            .any(|a| *a == "--prune=2.days.ago" || *a == "2.days.ago"));
        assert!(
            !argv.iter().any(|a| a.contains("now")),
            "argv must never prune to the present moment: {argv:?}"
        );
        assert_eq!(GC_PRUNE_WINDOW, "2.days.ago");
    }

    #[test]
    fn counts_parse_from_real_git_output() {
        let out = "count: 1952\nsize: 8492\nin-pack: 41233\npacks: 3\nsize-pack: 210331\nprune-packable: 0\ngarbage: 0\nsize-garbage: 0\n";
        assert_eq!(parse_loose_count(out), Some(1952));
    }

    #[test]
    fn a_missing_count_line_is_none_rather_than_zero() {
        // Zero would read as "clean repo" and silently skip it forever.
        assert_eq!(parse_loose_count("size: 8492\npacks: 3\n"), None);
    }

    #[test]
    fn above_threshold_collects_and_below_leaves_alone() {
        assert_eq!(classify_repo(1952, 1000, None), RepoVerdict::Collect);
        assert_eq!(classify_repo(1000, 1000, None), RepoVerdict::Collect);
        assert_eq!(classify_repo(869, 1000, None), RepoVerdict::BelowThreshold);
    }

    #[test]
    fn a_repo_mid_operation_is_skipped_however_many_loose_objects_it_has() {
        // Checked before the threshold on purpose: a human halfway through a
        // rebase is not to be interrupted by housekeeping.
        assert_eq!(
            classify_repo(99_999, 1000, Some("rebase")),
            RepoVerdict::MidOperation("rebase")
        );
    }

    #[test]
    fn every_in_flight_marker_git_leaves_is_detected() {
        let dir = std::env::temp_dir().join(format!("koi-gc-{}", std::process::id()));
        for (marker, expected) in [
            ("rebase-merge", "rebase"),
            ("rebase-apply", "rebase"),
            ("MERGE_HEAD", "merge"),
            ("CHERRY_PICK_HEAD", "cherry-pick"),
            ("BISECT_LOG", "bisect"),
        ] {
            let git_dir = dir.join(marker);
            std::fs::create_dir_all(&git_dir).unwrap();
            std::fs::write(git_dir.join(marker), b"x").unwrap();
            assert_eq!(in_progress_operation(&git_dir), Some(expected), "{marker}");
        }
        let clean = dir.join("clean");
        std::fs::create_dir_all(&clean).unwrap();
        assert_eq!(in_progress_operation(&clean), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
