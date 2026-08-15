//! Owned notes storage — plain Markdown files in ~/notes/.
//!
//! Notes are just `.md` files with optional YAML frontmatter. No cloud, no
//! proprietary format, no lock-in. The notes root is automatically marked as
//! a koi-managed zone so FileMonitor does not generate filing proposals for it.

use anyhow::{Context as _, Result};
use chrono::{DateTime, Local};
use std::path::{Path, PathBuf};

/// Location of the notes vault: ~/notes/
pub fn default_notes_dir() -> Option<PathBuf> {
    crate::state::home_dir().ok().map(|h| h.join("notes"))
}

/// A single note, parsed from a Markdown file.
#[derive(Debug, Clone)]
pub struct Note {
    pub path: PathBuf,
    pub title: String,
    pub created: DateTime<Local>,
    pub modified: DateTime<Local>,
    /// Body text including frontmatter.
    pub body: String,
}

/// Create a new note file with a timestamped filename.
///
/// Returns the path of the created file.
pub fn create_note(root: &Path, title: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(root).context("create notes directory")?;

    // Ensure the managed-zone marker exists so FileMonitor leaves notes alone.
    ensure_managed_zone(root)?;

    let now = Local::now();
    let slug = slugify(title);
    let filename = format!("{}-{}.md", now.format("%Y%m%d-%H%M%S"), slug);
    let path = root.join(&filename);

    let frontmatter = format!(
        "---\ntitle: {}\ncreated: {}\n---\n\n",
        title,
        now.to_rfc3339()
    );
    std::fs::write(&path, frontmatter).with_context(|| format!("write {}", path.display()))?;

    Ok(path)
}

/// List all notes in `root`, newest first.
pub fn list_notes(root: &Path) -> Result<Vec<Note>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut notes = Vec::new();
    for entry in std::fs::read_dir(root).context("read notes directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Ok(note) = read_note(&path) {
            notes.push(note);
        }
    }
    notes.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(notes)
}

/// Search notes for `query` (case-insensitive substring match on title + body).
pub fn search_notes(root: &Path, query: &str) -> Result<Vec<Note>> {
    let lower = query.to_ascii_lowercase();
    let all = list_notes(root)?;
    Ok(all
        .into_iter()
        .filter(|n| {
            n.title.to_ascii_lowercase().contains(&lower)
                || n.body.to_ascii_lowercase().contains(&lower)
        })
        .collect())
}

fn read_note(path: &Path) -> Result<Note> {
    let body = std::fs::read_to_string(path)?;
    let meta = std::fs::metadata(path)?;

    let title = extract_frontmatter_title(&body)
        .or_else(|| {
            body.lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l.trim_start_matches("# ").to_owned())
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });

    let modified = meta
        .modified()
        .map(DateTime::<Local>::from)
        .unwrap_or_else(|_| Local::now());
    let created = meta
        .created()
        .map(DateTime::<Local>::from)
        .unwrap_or(modified);

    Ok(Note {
        path: path.to_owned(),
        title,
        created,
        modified,
        body,
    })
}

fn extract_frontmatter_title(body: &str) -> Option<String> {
    if !body.starts_with("---") {
        return None;
    }
    let end = body[3..].find("---")?;
    let fm = &body[3..3 + end];
    for line in fm.lines() {
        if let Some(val) = line.strip_prefix("title:") {
            return Some(val.trim().trim_matches('"').to_owned());
        }
    }
    None
}

fn slugify(title: &str) -> String {
    title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .fold(String::new(), |mut acc, c| {
            if c == '-' && acc.ends_with('-') {
                acc
            } else {
                acc.push(c);
                acc
            }
        })
        .trim_matches('-')
        .to_owned()
}

/// Write a `.koi-managed-by` marker in `root` so FileMonitor leaves notes alone.
fn ensure_managed_zone(root: &Path) -> Result<()> {
    let marker = root.join(".koi-managed-by");
    if marker.exists() {
        return Ok(());
    }
    let owner = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "koi".to_owned());
    let content = format!(
        "system = \"koi-notes\"\nscope = \"recursive\"\nowner = \"{owner}\"\ncontact = \"koi notes — managed by koi notes command\"\n"
    );
    std::fs::write(&marker, content)
        .with_context(|| format!("write managed-zone marker {}", marker.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn create_and_list_note() {
        let dir = TempDir::new().unwrap();
        let path = create_note(dir.path(), "My Test Note").unwrap();
        assert!(path.exists());
        let notes = list_notes(dir.path()).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "My Test Note");
    }

    #[test]
    fn search_finds_title_match() {
        let dir = TempDir::new().unwrap();
        create_note(dir.path(), "Budget for June").unwrap();
        create_note(dir.path(), "Medication log").unwrap();
        let results = search_notes(dir.path(), "budget").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Budget for June");
    }

    #[test]
    fn managed_zone_marker_written() {
        let dir = TempDir::new().unwrap();
        create_note(dir.path(), "Anything").unwrap();
        assert!(dir.path().join(".koi-managed-by").exists());
    }

    #[test]
    fn slugify_normalises_title() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        let s = slugify("My  Note -- Here");
        assert!(
            !s.contains("--"),
            "consecutive dashes should be collapsed: {s}"
        );
    }
}
