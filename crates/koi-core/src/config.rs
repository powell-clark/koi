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
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct FilingConfig {
    pub roots: RootsConfig,
    pub cadences: CadencesConfig,
    pub dedupe: DedupeConfig,
    pub trash: TrashConfig,
    pub taxonomy: TaxonomyConfig,
    /// Ordered content-aware rules, evaluated before the extension buckets
    /// (TASK-KOI246). Empty in the file means "use the shipped seed table";
    /// an operator who writes any `[[rules]]` entry replaces it wholesale, so
    /// what they read in the file is what runs.
    pub rules: Vec<crate::filing::rules::FilingRule>,
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

/// The named taxonomy: what "organised" means on this machine (TASK-KOI245).
///
/// Destinations are paths relative to the documents root, each carrying a
/// one-line description of what belongs there. The descriptions are not
/// decoration — they are what the operator reads in `koi scan --explain` to
/// spot a wrong bucket before anything moves, and what a future rule author
/// checks a match against.
///
/// Drafted from the real corpus rather than a generic template: see
/// `data/filing-inventory-2026-09-02.md` for the counts each destination was
/// chosen to serve.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TaxonomyConfig {
    /// Destination path (relative to the documents root) -> description.
    pub destinations: BTreeMap<String, String>,
}

/// Why a taxonomy is not usable. Returned by [`TaxonomyConfig::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaxonomyError {
    /// A destination carries no description, so nothing says what belongs there.
    MissingDescription { destination: String },
    /// Two destinations normalise to the same path, so filing is ambiguous.
    DuplicateDestination {
        normalised: String,
        first: String,
        second: String,
    },
}

impl std::fmt::Display for TaxonomyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDescription { destination } => {
                write!(f, "taxonomy destination `{destination}` has no description")
            }
            Self::DuplicateDestination {
                normalised,
                first,
                second,
            } => write!(
                f,
                "taxonomy destinations `{first}` and `{second}` both normalise to `{normalised}`"
            ),
        }
    }
}

impl TaxonomyConfig {
    /// Normalise for duplicate detection: case-folded, slash-trimmed, and
    /// backslashes folded to forward slashes so a Windows-style entry cannot
    /// silently shadow its POSIX twin.
    fn normalise(destination: &str) -> String {
        destination
            .replace('\\', "/")
            .split('/')
            .filter(|seg| !seg.is_empty())
            .map(|seg| seg.to_lowercase())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Every destination has a non-blank description and no two collide.
    pub fn validate(&self) -> Result<(), TaxonomyError> {
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for (destination, description) in &self.destinations {
            if description.trim().is_empty() {
                return Err(TaxonomyError::MissingDescription {
                    destination: destination.clone(),
                });
            }
            let key = Self::normalise(destination);
            if let Some(first) = seen.get(&key) {
                return Err(TaxonomyError::DuplicateDestination {
                    normalised: key,
                    first: first.clone(),
                    second: destination.clone(),
                });
            }
            seen.insert(key, destination.clone());
        }
        Ok(())
    }

    /// Destinations in reading order, paired with their descriptions.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.destinations
            .iter()
            .map(|(d, desc)| (d.as_str(), desc.as_str()))
    }
}

impl Default for TaxonomyConfig {
    /// The compiled default taxonomy. Every destination here was chosen
    /// against a measured count in the 2026-09-02 corpus inventory, so none of
    /// them is speculative.
    fn default() -> Self {
        let pairs = [
            (
                "Finance/Statements",
                "Bank and card statements from any issuer - Monzo, Starling, HSBC and the rest.",
            ),
            (
                "Finance/Invoices",
                "Invoices you issued or received, including PCL client invoices.",
            ),
            (
                "Finance/Receipts",
                "Purchase receipts and order confirmations kept for expenses.",
            ),
            (
                "Finance/Tax-and-Companies-House",
                "HMRC correspondence, self-assessment, VAT, and Companies House filings.",
            ),
            (
                "Health",
                "Medical results, patient statements, prescriptions and NHS correspondence.",
            ),
            (
                "Business/PCL",
                "Powell-Clark Limited business records that are not finance documents.",
            ),
            (
                "Personal/Identity",
                "Passport, driving licence, birth certificate and other identity documents.",
            ),
            (
                "Personal/Travel",
                "Bookings, itineraries, tickets and travel insurance.",
            ),
            ("Fonts", "Font files installed or kept for installation."),
            (
                "Software",
                "Installers and packages - .deb, .AppImage, .exe, .dmg and archives of them.",
            ),
            (
                "Media/Screenshots",
                "Screen captures, by far the largest single kind in the corpus.",
            ),
            (
                "Media/Photos",
                "Photographs and scanned images that are not screenshots.",
            ),
            (
                "Reference",
                "Documents worth keeping that belong to no other destination.",
            ),
        ];
        Self {
            destinations: pairs
                .into_iter()
                .map(|(d, desc)| (d.to_string(), desc.to_string()))
                .collect(),
        }
    }
}

impl FilingConfig {
    /// The rule table actually in force: the operator's `[[rules]]` if they
    /// wrote any, otherwise the shipped seed table.
    pub fn rule_set(&self) -> crate::filing::rules::RuleSet {
        if self.rules.is_empty() {
            crate::filing::rules::RuleSet::seed()
        } else {
            crate::filing::rules::RuleSet::new(self.rules.clone())
        }
    }

    /// The documents root the taxonomy hangs off: the configured override, or
    /// `$HOME/Documents`. Same resolution `DocumentsMonitor::from_config` uses,
    /// so `koi scan` cannot create the taxonomy under a different root from the
    /// one it then files into.
    pub fn documents_root(&self) -> std::io::Result<PathBuf> {
        let home = crate::state::home_dir()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))?;
        Ok(self
            .roots
            .documents
            .clone()
            .unwrap_or_else(|| home.join("Documents")))
    }

    /// Create every taxonomy destination under the documents root, skipping
    /// any that already exists. Idempotent, and a managed zone is never
    /// written into: a destination whose parent chain carries a
    /// `.koi-managed-by` marker is reported as skipped rather than created.
    ///
    /// Returns (created, skipped_existing, skipped_managed).
    pub fn ensure_taxonomy_dirs(&self) -> std::io::Result<TaxonomyDirReport> {
        let root = self.documents_root()?;
        let mut report = TaxonomyDirReport::default();
        for (destination, _) in self.taxonomy.entries() {
            let path = root.join(destination);
            if path.is_dir() {
                report.existing.push(path);
                continue;
            }
            if crate::filing::managed_zone::load_zone(&root).is_some() {
                report.managed.push(path);
                continue;
            }
            std::fs::create_dir_all(&path)?;
            report.created.push(path);
        }
        Ok(report)
    }
}

/// What [`FilingConfig::ensure_taxonomy_dirs`] did, so a caller can report it
/// rather than guess.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TaxonomyDirReport {
    pub created: Vec<PathBuf>,
    pub existing: Vec<PathBuf>,
    pub managed: Vec<PathBuf>,
}

impl FilingConfig {
    /// Load from `$HOME/.config/koi/filing.toml`. `$HOME` unset behaves the
    /// same as a missing file: compiled defaults, no warning.
    pub fn load() -> Self {
        match crate::state::home_dir() {
            Ok(home) => Self::load_from(&home.join(".config/koi/filing.toml")),
            Err(_) => Self::default(),
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

    #[test]
    fn default_taxonomy_is_valid() {
        TaxonomyConfig::default()
            .validate()
            .expect("the compiled default taxonomy must satisfy its own rules");
    }

    #[test]
    fn default_taxonomy_carries_every_required_destination() {
        // The set TASK-KOI245 AC-2 names. A destination dropped here is a
        // silent regression: files route to Reference instead of their bucket.
        let taxonomy = TaxonomyConfig::default();
        for required in [
            "Finance/Statements",
            "Finance/Invoices",
            "Finance/Receipts",
            "Finance/Tax-and-Companies-House",
            "Health",
            "Business/PCL",
            "Personal/Identity",
            "Personal/Travel",
            "Fonts",
            "Software",
            "Media/Screenshots",
            "Media/Photos",
            "Reference",
        ] {
            assert!(
                taxonomy.destinations.contains_key(required),
                "taxonomy is missing required destination {required}"
            );
        }
    }

    #[test]
    fn taxonomy_rejects_a_blank_description() {
        let mut taxonomy = TaxonomyConfig::default();
        taxonomy
            .destinations
            .insert("Finance/Nothing".to_string(), "   ".to_string());
        assert_eq!(
            taxonomy.validate(),
            Err(TaxonomyError::MissingDescription {
                destination: "Finance/Nothing".to_string()
            })
        );
    }

    #[test]
    fn taxonomy_rejects_two_destinations_that_normalise_alike() {
        // TOML cannot express a literal duplicate key, so the collision that
        // actually reaches us is a case or trailing-slash variant.
        let mut taxonomy = TaxonomyConfig::default();
        taxonomy.destinations.insert(
            "finance/statements/".to_string(),
            "a second bucket".to_string(),
        );
        match taxonomy.validate() {
            Err(TaxonomyError::DuplicateDestination { normalised, .. }) => {
                assert_eq!(normalised, "finance/statements");
            }
            other => panic!("expected a duplicate-destination error, got {other:?}"),
        }
    }

    #[test]
    fn taxonomy_loads_from_toml_and_overrides_the_defaults() {
        let dir = std::env::temp_dir().join(format!("koi-tax-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("filing.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "[taxonomy.destinations]\n\"Archive/Ships-Logs\" = \"logs from the ships\""
        )
        .unwrap();
        let cfg = FilingConfig::load_from(&path);
        assert_eq!(
            cfg.taxonomy
                .destinations
                .get("Archive/Ships-Logs")
                .map(String::as_str),
            Some("logs from the ships")
        );
        cfg.taxonomy
            .validate()
            .expect("operator taxonomy should validate");
        std::fs::remove_dir_all(&dir).ok();
    }
}
