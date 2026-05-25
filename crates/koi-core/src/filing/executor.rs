//! Proposal executor — applies approved actions.
//!
//! Per ADR-0014, every mutation is gated by an Approval decision. The
//! executor is the ONLY code path that moves user files. It always preserves
//! the original filename (no auto-rename on conflict), never overwrites, and
//! creates destination directories as needed.

use std::{fs, path::Path};

use crate::{error::Error, filing::ProposedAction, Result};

/// Result of applying a single proposal.
#[derive(Debug, Clone)]
pub enum Outcome {
    Applied,
    Skipped(String),
    Failed(String),
}

pub fn apply(source: &Path, action: &ProposedAction) -> Outcome {
    match action {
        ProposedAction::Move { dest } => apply_move(source, dest),
        ProposedAction::Archive { archive_root } => {
            let Some(filename) = source.file_name() else {
                return Outcome::Failed("source has no filename".into());
            };
            apply_move(source, &archive_root.join(filename))
        }
        ProposedAction::Delete => apply_delete(source),
        ProposedAction::Tag { labels } => apply_tag(source, labels),
        ProposedAction::Ignore { .. } => Outcome::Skipped("ignore is metadata-only".into()),
        ProposedAction::DriveMove {
            remote_src,
            remote_dest,
        } => apply_drive_move(remote_src, remote_dest),
    }
}

fn apply_move(source: &Path, dest: &Path) -> Outcome {
    if !source.exists() {
        return Outcome::Skipped(format!("source gone: {}", source.display()));
    }
    if dest.exists() {
        return Outcome::Skipped(format!("dest exists (no overwrite): {}", dest.display()));
    }
    if let Some(parent) = dest.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return Outcome::Failed(format!("mkdir {}: {e}", parent.display()));
        }
    }
    match fs::rename(source, dest) {
        Ok(()) => Outcome::Applied,
        Err(e) if e.raw_os_error() == Some(18) /* EXDEV */ => {
            // cross-device: copy + remove
            match fs::copy(source, dest).and_then(|_| fs::remove_file(source)) {
                Ok(()) => Outcome::Applied,
                Err(e2) => {
                    let _ = fs::remove_file(dest);
                    Outcome::Failed(format!("cross-device move failed: {e2}"))
                }
            }
        }
        Err(e) => Outcome::Failed(format!("rename failed: {e}")),
    }
}

fn apply_delete(_source: &Path) -> Outcome {
    // Delete proposals are AutonomyTier::Human — they should never reach the
    // executor via automated approval, only via explicit human invocation.
    // Refuse to delete through this path; a dedicated human-initiated delete
    // command will own that responsibility.
    Outcome::Skipped("delete not executed via standard approval loop (Human tier)".into())
}

fn apply_tag(_source: &Path, _labels: &[String]) -> Outcome {
    // xattr support varies by filesystem; defer to a dedicated story.
    Outcome::Skipped("tag action not yet implemented".into())
}

fn apply_drive_move(remote_src: &str, remote_dest: &str) -> Outcome {
    let output = match std::process::Command::new("rclone")
        .args(["moveto", remote_src, remote_dest])
        .output()
    {
        Ok(o) => o,
        Err(e) => return Outcome::Failed(format!("rclone not found: {e}")),
    };
    if output.status.success() {
        Outcome::Applied
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Outcome::Failed(format!("rclone moveto failed: {stderr}"))
    }
}

/// Apply an action against a source path, returning a `Result`-shaped
/// outcome for callers that want error propagation semantics.
pub fn apply_or_err(source: &Path, action: &ProposedAction) -> Result<()> {
    match apply(source, action) {
        Outcome::Applied => Ok(()),
        Outcome::Skipped(why) => Err(Error::Config(format!("skipped: {why}"))),
        Outcome::Failed(why) => Err(Error::Config(format!("failed: {why}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(prefix: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("koi-exec-{prefix}-{nanos:x}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn applies_simple_move() {
        let scratch = tmp("move");
        let src = scratch.join("src.pdf");
        fs::write(&src, b"hello").unwrap();
        let dest = scratch.join("sub/dst.pdf");
        let outcome = apply(&src, &ProposedAction::Move { dest: dest.clone() });
        assert!(matches!(outcome, Outcome::Applied));
        assert!(!src.exists());
        assert!(dest.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
        fs::remove_dir_all(&scratch).ok();
    }

    #[test]
    fn refuses_to_overwrite() {
        let scratch = tmp("overwrite");
        let src = scratch.join("s.pdf");
        let dst = scratch.join("d.pdf");
        fs::write(&src, b"new").unwrap();
        fs::write(&dst, b"old").unwrap();
        let outcome = apply(&src, &ProposedAction::Move { dest: dst.clone() });
        assert!(matches!(outcome, Outcome::Skipped(_)));
        assert!(src.exists(), "src must remain when refusing overwrite");
        assert_eq!(fs::read(&dst).unwrap(), b"old");
        fs::remove_dir_all(&scratch).ok();
    }

    #[test]
    fn delete_is_refused_by_default() {
        let scratch = tmp("del");
        let src = scratch.join("x");
        fs::write(&src, b"").unwrap();
        let outcome = apply(&src, &ProposedAction::Delete);
        assert!(matches!(outcome, Outcome::Skipped(_)));
        assert!(
            src.exists(),
            "delete path must not remove via standard approval"
        );
        fs::remove_dir_all(&scratch).ok();
    }

    #[test]
    fn missing_source_is_skipped_not_failed() {
        let outcome = apply(
            Path::new("/tmp/does-not-exist-zzzzzz"),
            &ProposedAction::Move {
                dest: PathBuf::from("/tmp/anywhere"),
            },
        );
        assert!(matches!(outcome, Outcome::Skipped(_)));
    }
}
