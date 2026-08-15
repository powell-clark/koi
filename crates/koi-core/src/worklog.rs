//! Shared worklog-writer — append-only JSONL entries recording executed work
//! (TASK-KOI190), so any CLI command can record what it did the same way this
//! session's manual sweeps have (WORK-KOI041 etc.), not just `koi clean`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{state::default_data_dir, Result};

/// One executed-work record, matching the WORK-KOI### schema already in use
/// across data/worklog.jsonl (id, timestamp, session_id, title, changes) plus
/// an optional task reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorklogEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub changes: Vec<String>,
}

/// The koi data directory's worklog.jsonl (per ADR-0019 — runtime state
/// resolves via XDG dirs, not the source tree the binary happens to run in).
pub fn worklog_path() -> Result<PathBuf> {
    Ok(default_data_dir()?.join("worklog.jsonl"))
}

/// Append one entry to the worklog at `path`, minting its id from the file's
/// current contents. Returns the minted id.
pub fn append(
    path: &Path,
    session_id: &str,
    title: &str,
    task_ref: Option<&str>,
    kind: &str,
    changes: Vec<String>,
) -> Result<String> {
    let id = format!("WORK-KOI{:03}", next_id(path));
    let entry = WorklogEntry {
        id: id.clone(),
        timestamp: Utc::now(),
        session_id: session_id.to_string(),
        title: title.to_string(),
        task_ref: task_ref.map(str::to_string),
        kind: kind.to_string(),
        changes,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", serde_json::to_string(&entry)?)?;
    Ok(id)
}

/// Next sequential WORK-KOI### number, one past the highest id already
/// present in `path`. A missing or empty file starts numbering at 1.
pub fn next_id(path: &Path) -> u32 {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 1;
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|v| v.get("id")?.as_str().map(str::to_string))
        .filter_map(|id| id.strip_prefix("WORK-KOI")?.parse::<u32>().ok())
        .max()
        .map_or(1, |max| max + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_id_starts_at_one_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worklog.jsonl");
        assert_eq!(next_id(&path), 1);
    }

    #[test]
    fn next_id_returns_one_past_highest_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worklog.jsonl");
        std::fs::write(
            &path,
            "{\"id\":\"WORK-KOI001\"}\n{\"id\":\"WORK-KOI003\"}\n{\"id\":\"WORK-KOI002\"}\n",
        )
        .unwrap();
        assert_eq!(next_id(&path), 4);
    }

    #[test]
    fn append_writes_entry_with_minted_id_and_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worklog.jsonl");

        let id = append(
            &path,
            "koi-cli",
            "koi clean freed 3 targets",
            None,
            "maintenance",
            vec!["cleared ~/.cache/foo (1.2G)".to_string()],
        )
        .unwrap();

        assert_eq!(id, "WORK-KOI001");
        let text = std::fs::read_to_string(&path).unwrap();
        let line = text.lines().next().unwrap();
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(entry["id"], "WORK-KOI001");
        assert_eq!(entry["session_id"], "koi-cli");
        assert_eq!(entry["title"], "koi clean freed 3 targets");
        assert_eq!(entry["type"], "maintenance");
        assert_eq!(entry["changes"][0], "cleared ~/.cache/foo (1.2G)");
    }

    #[test]
    fn append_is_sequential_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worklog.jsonl");

        let first = append(&path, "koi-cli", "first", None, "maintenance", vec![]).unwrap();
        let second = append(&path, "koi-cli", "second", None, "maintenance", vec![]).unwrap();

        assert_eq!(first, "WORK-KOI001");
        assert_eq!(second, "WORK-KOI002");
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
    }
}
