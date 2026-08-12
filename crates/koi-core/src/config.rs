//! Config koi actually reads — `~/.config/koi/filing.toml`, same path
//! convention as `filing::gdrive`'s `~/.config/koi/gdrive.json`. Scan roots
//! and re-scan cadences today; dedupe thresholds and trash retention join
//! this struct when `koi dedupe`/`koi trash` exist to consume them
//! (TASK-KOI209/TASK-KOI210).
//!
//! A missing file is the expected default state — no warning. A malformed
//! file falls back to compiled defaults with a warning; it must never crash
//! the daemon.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct FilingConfig {
    pub roots: RootsConfig,
    pub cadences: CadencesConfig,
    pub dedupe: DedupeConfig,
    pub trash: TrashConfig,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct RootsConfig {
    pub downloads: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub inbox: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CadencesConfig {
    pub downloads_hours: u64,
    pub documents_hours: u64,
    pub inbox_hours: u64,
    pub root_clutter_hours: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DedupeConfig {
    /// Files larger than this are skipped by `koi dedupe scan` (ADR-0021).
    pub max_size_mb: u64,
    /// How often `koi-daemon` runs an unattended `dedupe::scan` (FEAT-KOI054 AC-7).
    pub scan_interval_days: u64,
}

impl Default for DedupeConfig {
    fn default() -> Self {
        Self {
            max_size_mb: 100,
            scan_interval_days: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TrashConfig {
    /// Default `koi trash empty --older-than` window when the flag is
    /// omitted. Never applied automatically — `trash empty` is always a
    /// human-initiated CLI invocation (ADR-0021).
    pub retention_days: u64,
}

impl Default for TrashConfig {
    fn default() -> Self {
        Self { retention_days: 30 }
    }
}

impl Default for CadencesConfig {
    fn default() -> Self {
        // Matches the hardcoded values koi-daemon used before this config
        // existed — changing these defaults changes daemon behaviour for
        // every operator without a filing.toml, so treat them as load-bearing.
        Self {
            downloads_hours: 24,
            documents_hours: 24,
            inbox_hours: 6,
            root_clutter_hours: 24,
        }
    }
}

impl FilingConfig {
    /// Load from `$HOME/.config/koi/filing.toml`. `$HOME` unset behaves the
    /// same as a missing file: compiled defaults, no warning.
    pub fn load() -> Self {
        match std::env::var_os("HOME") {
            Some(home) => Self::load_from(&PathBuf::from(home).join(".config/koi/filing.toml")),
            None => Self::default(),
        }
    }

    /// Load from an explicit path — the testable half of [`Self::load`].
    pub fn load_from(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Self::default(),
        };
        match toml::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "malformed filing.toml, falling back to compiled defaults"
                );
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_toml(prefix: &str, contents: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("koi-filing-config-{prefix}-{nanos:x}.toml"));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn default_cadences_match_the_values_the_daemon_used_to_hardcode() {
        let cfg = FilingConfig::default();
        assert_eq!(cfg.cadences.downloads_hours, 24);
        assert_eq!(cfg.cadences.documents_hours, 24);
        assert_eq!(cfg.cadences.inbox_hours, 6);
        assert_eq!(cfg.cadences.root_clutter_hours, 24);
        assert_eq!(cfg.roots, RootsConfig::default());
    }

    #[test]
    fn default_dedupe_max_size_matches_adr_0021() {
        assert_eq!(FilingConfig::default().dedupe.max_size_mb, 100);
    }

    #[test]
    fn dedupe_max_size_is_overridable() {
        let path = tmp_toml(
            "dedupe-override",
            r#"
            [dedupe]
            max_size_mb = 250
            "#,
        );
        let cfg = FilingConfig::load_from(&path);
        assert_eq!(cfg.dedupe.max_size_mb, 250);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn default_dedupe_scan_interval_is_monthly() {
        assert_eq!(FilingConfig::default().dedupe.scan_interval_days, 30);
    }

    #[test]
    fn dedupe_scan_interval_is_overridable() {
        let path = tmp_toml(
            "dedupe-interval-override",
            r#"
            [dedupe]
            scan_interval_days = 7
            "#,
        );
        let cfg = FilingConfig::load_from(&path);
        assert_eq!(cfg.dedupe.scan_interval_days, 7);
        // Untouched field keeps its default alongside the override.
        assert_eq!(cfg.dedupe.max_size_mb, 100);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn default_trash_retention_is_monthly() {
        assert_eq!(FilingConfig::default().trash.retention_days, 30);
    }

    #[test]
    fn trash_retention_is_overridable() {
        let path = tmp_toml(
            "trash-retention-override",
            r#"
            [trash]
            retention_days = 14
            "#,
        );
        let cfg = FilingConfig::load_from(&path);
        assert_eq!(cfg.trash.retention_days, 14);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_returns_defaults_without_panicking() {
        let path = std::env::temp_dir().join("koi-filing-config-does-not-exist.toml");
        assert!(!path.exists());
        assert_eq!(FilingConfig::load_from(&path), FilingConfig::default());
    }

    #[test]
    fn malformed_file_falls_back_to_defaults_without_panicking() {
        let path = tmp_toml("malformed", "this is not valid toml {{{");
        assert_eq!(FilingConfig::load_from(&path), FilingConfig::default());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn overridden_root_and_cadence_are_read_from_the_file() {
        let path = tmp_toml(
            "override",
            r#"
            [roots]
            downloads = "/mnt/scratch/Downloads"

            [cadences]
            inbox_hours = 1
            "#,
        );
        let cfg = FilingConfig::load_from(&path);
        assert_eq!(
            cfg.roots.downloads,
            Some(PathBuf::from("/mnt/scratch/Downloads"))
        );
        // Fields absent from the file keep their compiled defaults.
        assert_eq!(cfg.roots.documents, None);
        assert_eq!(cfg.cadences.inbox_hours, 1);
        assert_eq!(cfg.cadences.downloads_hours, 24);
        std::fs::remove_file(&path).ok();
    }
}
