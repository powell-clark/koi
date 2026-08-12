//! Reversible trash — the only place a dedupe "remove this duplicate" verb
//! lands. See ADR-0021: trash is a move, never a delete. Both directions
//! reuse the existing overwrite-refusing, EXDEV-safe `executor::apply` — no
//! new mutation primitive is added anywhere in this module.
//!
//! `koi trash empty` (in koi-cli, not here) is the only delete-shaped
//! operation this capability has, and it never touches this module's move
//! functions — it removes already-trashed files directly, human-initiated,
//! confirmed, never scheduled.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::{
    error::Error,
    filing::{executor, ProposedAction},
    Result,
};

/// Default trash root: `~/.local/share/koi/trash` (same XDG data dir
/// `state.rs` uses for `koi.sqlite`).
pub fn default_trash_root() -> Result<PathBuf> {
    Ok(crate::state::default_data_dir()?.join("trash"))
}

/// Move `source` into `<trash_root>/<ISO-date>/<path-relative-to-home>`.
/// Falls back to the path stripped of its leading `/` if `source` is not
/// under `home` (koi's scan roots are always under `$HOME`, so this is a
/// defensive fallback, not the expected path).
pub fn move_to_trash(
    source: &Path,
    trash_root: &Path,
    home: &Path,
    now: DateTime<Utc>,
) -> Result<PathBuf> {
    let rel = source
        .strip_prefix(home)
        .unwrap_or_else(|_| source.strip_prefix("/").unwrap_or(source));
    let dest = trash_root
        .join(now.format("%Y-%m-%d").to_string())
        .join(rel);
    match executor::apply(source, &ProposedAction::Move { dest: dest.clone() }) {
        executor::Outcome::Applied => Ok(dest),
        executor::Outcome::Skipped(why) => Err(Error::Config(format!("trash move skipped: {why}"))),
        executor::Outcome::Failed(why) => Err(Error::Config(format!("trash move failed: {why}"))),
    }
}

/// Move a trashed file back to its original location — the exact inverse of
/// [`move_to_trash`], via the same executor path.
pub fn restore_from_trash(trash_path: &Path, original_path: &Path) -> Result<()> {
    match executor::apply(
        trash_path,
        &ProposedAction::Move {
            dest: original_path.to_path_buf(),
        },
    ) {
        executor::Outcome::Applied => Ok(()),
        executor::Outcome::Skipped(why) => Err(Error::Config(format!("restore skipped: {why}"))),
        executor::Outcome::Failed(why) => Err(Error::Config(format!("restore failed: {why}"))),
    }
}

/// Parse a `koi trash empty --older-than` window like `"30d"`, `"12h"`,
/// `"90m"` into a [`chrono::Duration`]. Accepts `d`/`h`/`m` suffixes only —
/// deliberately minimal, no calendar-aware units (months/years).
pub fn parse_older_than(s: &str) -> Result<chrono::Duration> {
    let s = s.trim();
    let (num_part, unit) = s.split_at(s.len().saturating_sub(1));
    let n: i64 = num_part
        .parse()
        .map_err(|_| Error::Config(format!("invalid --older-than value: {s:?}")))?;
    match unit {
        "d" => Ok(chrono::Duration::days(n)),
        "h" => Ok(chrono::Duration::hours(n)),
        "m" => Ok(chrono::Duration::minutes(n)),
        _ => Err(Error::Config(format!(
            "invalid --older-than unit in {s:?} — use a d/h/m suffix, e.g. 30d"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(prefix: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("koi-trash-{prefix}-{nanos:x}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn checksum(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }

    #[test]
    fn move_to_trash_lands_at_dated_home_relative_path() {
        let home = tmpdir("home");
        fs::create_dir_all(home.join("Downloads")).unwrap();
        let source = home.join("Downloads/dupe.pdf");
        fs::write(&source, b"trash me").unwrap();
        let trash_root = tmpdir("trashroot");
        let now = "2026-08-12T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let dest = move_to_trash(&source, &trash_root, &home, now).unwrap();

        assert_eq!(dest, trash_root.join("2026-08-12/Downloads/dupe.pdf"));
        assert!(!source.exists());
        assert!(dest.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"trash me");

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&trash_root).ok();
    }

    #[test]
    fn trash_then_restore_round_trips_byte_identical() {
        let home = tmpdir("home2");
        let source = home.join("original.bin");
        let content: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
        fs::write(&source, &content).unwrap();
        let original_checksum = checksum(&content);

        let trash_root = tmpdir("trashroot2");
        let now = Utc::now();
        let trashed = move_to_trash(&source, &trash_root, &home, now).unwrap();
        assert!(!source.exists());

        restore_from_trash(&trashed, &source).unwrap();

        assert!(source.exists());
        assert!(!trashed.exists());
        assert_eq!(checksum(&fs::read(&source).unwrap()), original_checksum);

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&trash_root).ok();
    }

    #[test]
    fn trashing_the_same_relative_path_twice_in_one_day_never_overwrites() {
        // Same overwrite-refusal executor::apply already guarantees for
        // ordinary moves — proved here for the trash path specifically,
        // since a second duplicate with the same filename in the same
        // source directory on the same day is a realistic collision.
        let home = tmpdir("home3");
        fs::create_dir_all(home.join("Downloads")).unwrap();
        let trash_root = tmpdir("trashroot3");
        let now = "2026-08-12T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let first = home.join("Downloads/report.pdf");
        fs::write(&first, b"first").unwrap();
        move_to_trash(&first, &trash_root, &home, now).unwrap();

        // A second, unrelated file that happens to share the trash
        // destination path (e.g. re-downloaded with the same name).
        fs::write(&first, b"second").unwrap();
        let result = move_to_trash(&first, &trash_root, &home, now);

        assert!(
            result.is_err(),
            "second trash of the same path must not silently overwrite the first"
        );
        assert!(
            first.exists(),
            "source must remain when the trash move is refused"
        );
        assert_eq!(fs::read(&first).unwrap(), b"second");
        assert_eq!(
            fs::read(trash_root.join("2026-08-12/Downloads/report.pdf")).unwrap(),
            b"first",
            "the original trashed copy must be untouched"
        );

        fs::remove_dir_all(&home).ok();
        fs::remove_dir_all(&trash_root).ok();
    }

    #[test]
    fn parse_older_than_accepts_days_hours_minutes() {
        assert_eq!(parse_older_than("30d").unwrap(), chrono::Duration::days(30));
        assert_eq!(
            parse_older_than("12h").unwrap(),
            chrono::Duration::hours(12)
        );
        assert_eq!(
            parse_older_than("90m").unwrap(),
            chrono::Duration::minutes(90)
        );
    }

    #[test]
    fn parse_older_than_rejects_unknown_units_and_garbage() {
        assert!(parse_older_than("30").is_err());
        assert!(parse_older_than("30y").is_err());
        assert!(parse_older_than("xd").is_err());
    }
}
