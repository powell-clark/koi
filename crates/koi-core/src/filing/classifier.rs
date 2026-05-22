//! Classifier — replaces seed rules once enough decisions accumulate.
//!
//! v0: SQL-counted approvals vs. rejections keyed on (monitor, filename suffix).
//! See ADR-0014 for the learning-loop design. The trait lets FileMonitors stay
//! storage-agnostic (unit tests pass a stub; production passes SqliteClassifier).

use std::{path::PathBuf, sync::Mutex};

use rusqlite::Connection;

/// Classification query: "given this file and this monitor, where has the
/// user said it should go, and how confident are we?"
pub trait Classifier: Send + Sync {
    /// Return (destination_directory, confidence) if enough signal exists.
    fn suggest(&self, monitor: &str, suffix: &str) -> Option<(PathBuf, f32)>;
}

/// SQLite-backed classifier — thin wrapper around `state::learned_destination`.
pub struct SqliteClassifier {
    conn: Mutex<Connection>,
}

impl SqliteClassifier {
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }
}

impl Classifier for SqliteClassifier {
    fn suggest(&self, monitor: &str, suffix: &str) -> Option<(PathBuf, f32)> {
        let guard = self.conn.lock().ok()?;
        crate::state::learned_destination(&guard, monitor, suffix)
            .ok()
            .flatten()
    }
}
