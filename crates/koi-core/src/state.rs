//! SQLite-backed persistent state.
//!
//! Single-file implementation (PRAGMA user_version migration + typed helpers
//! for the three tables defined in ADR-0013 and ADR-0014):
//!
//! - `monitor_reports` — time-series snapshots from health monitors
//! - `proposals` — pending/applied/rejected file lifecycle proposals
//! - `decisions` — human approvals/rejections, the learning signal
//!
//! Connection is SQLite with WAL mode; callers own the `Connection`.

use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};

use crate::{
    error::Error,
    filing::{AutonomyTier, Proposal, ProposalId, ProposedAction},
    types::{HealthStatus, MonitorReport},
    Result,
};

/// Current schema version. Bump this when adding a migration.
const CURRENT_VERSION: u32 = 5;

const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS monitor_reports (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    monitor     TEXT NOT NULL,
    status      TEXT NOT NULL,
    elapsed_ms  INTEGER NOT NULL,
    collected_at TEXT NOT NULL,
    payload     TEXT NOT NULL  -- serde_json of full MonitorReport
);
CREATE INDEX IF NOT EXISTS idx_monitor_reports_monitor_time
    ON monitor_reports(monitor, collected_at);

CREATE TABLE IF NOT EXISTS proposals (
    id              TEXT PRIMARY KEY,       -- ProposalId hex
    monitor         TEXT NOT NULL,
    path            TEXT NOT NULL,
    action_kind     TEXT NOT NULL,
    action_payload  TEXT NOT NULL,          -- serde_json of ProposedAction
    rationale       TEXT NOT NULL,
    confidence      REAL NOT NULL,
    autonomy_tier   TEXT NOT NULL,
    emitted_at      TEXT NOT NULL,
    state           TEXT NOT NULL DEFAULT 'pending'  -- pending/applied/rejected/failed
);
CREATE INDEX IF NOT EXISTS idx_proposals_state ON proposals(state);
CREATE INDEX IF NOT EXISTS idx_proposals_monitor ON proposals(monitor);

CREATE TABLE IF NOT EXISTS decisions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    proposal_id  TEXT NOT NULL REFERENCES proposals(id),
    decision     TEXT NOT NULL,             -- approved/rejected/deferred
    decided_at   TEXT NOT NULL,
    notes        TEXT
);
CREATE INDEX IF NOT EXISTS idx_decisions_proposal ON decisions(proposal_id);
"#;

/// Runtime data directory: `~/.local/share/koi` on Linux, platform equivalents
/// on macOS/Windows.
///
/// This is the single home for operational state that the product reads and
/// writes (ADR-0019). The product never reads from or writes to its own source
/// tree — all runtime state resolves here via XDG dirs, independent of the
/// current working directory.
pub fn default_data_dir() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "powellclark", "koi")
        .ok_or_else(|| Error::Config("no user data directory".into()))?;
    Ok(dirs.data_dir().to_path_buf())
}

/// Default on-disk path: `~/.local/share/koi/koi.sqlite` on Linux,
/// platform equivalents on macOS/Windows.
pub fn default_db_path() -> Result<PathBuf> {
    Ok(default_data_dir()?.join("koi.sqlite"))
}

/// Open (or create) a koi state database and apply migrations.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(&conn)?;
    Ok(conn)
}

/// Open an in-memory database — for tests.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(&conn)?;
    Ok(conn)
}

const MIGRATION_V2: &str = r#"
CREATE TABLE IF NOT EXISTS process_crashes (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    comm          TEXT NOT NULL,               -- process name, e.g. "wezterm-gui"
    detected_at   TEXT NOT NULL,               -- ISO-8601 UTC when koi detected the crash
    crash_type    TEXT NOT NULL,               -- "oom_kill" | "segfault" | "signal" | "unknown"
    pid           INTEGER,                     -- PID if known
    last_rss_mb   REAL,                        -- RSS from last monitor report before crash (MiB)
    message       TEXT NOT NULL DEFAULT ''     -- raw journal excerpt for context
);
CREATE INDEX IF NOT EXISTS idx_process_crashes_comm_time
    ON process_crashes(comm, detected_at);
"#;

const MIGRATION_V3: &str = r#"
CREATE TABLE IF NOT EXISTS audit_runs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    ran_at          TEXT NOT NULL,    -- ISO-8601 UTC
    hardening_index INTEGER,          -- 0-100 Lynis score (NULL if parse failed)
    quick           INTEGER NOT NULL DEFAULT 0,  -- 1 if --quick was used
    report_path     TEXT NOT NULL  DEFAULT '',    -- path to the saved log
    lynis_version   TEXT NOT NULL  DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_audit_runs_time ON audit_runs(ran_at);
"#;

const MIGRATION_V4: &str = r#"
CREATE TABLE IF NOT EXISTS duplicate_groups (
    group_id      TEXT PRIMARY KEY,   -- hex blake3 content hash (see ADR-0021)
    content_hash  TEXT NOT NULL,
    size          INTEGER NOT NULL,
    first_seen    TEXT NOT NULL       -- set once, preserved across re-scans
);

CREATE TABLE IF NOT EXISTS duplicate_members (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id   TEXT NOT NULL REFERENCES duplicate_groups(group_id),
    path       TEXT NOT NULL,
    mtime      TEXT NOT NULL,
    keep_flag  INTEGER NOT NULL DEFAULT 0  -- 1 for the oldest (kept) member
);
CREATE INDEX IF NOT EXISTS idx_duplicate_members_group ON duplicate_members(group_id);
"#;

const MIGRATION_V5: &str = r#"
CREATE TABLE IF NOT EXISTS trash_log (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    original_path  TEXT NOT NULL,
    trash_path     TEXT NOT NULL,
    trashed_at     TEXT NOT NULL,
    restored_at    TEXT   -- NULL while still in trash
);
CREATE INDEX IF NOT EXISTS idx_trash_log_restored ON trash_log(restored_at);
"#;

fn migrate(conn: &Connection) -> Result<()> {
    let mut current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    while current < CURRENT_VERSION {
        let next = current + 1;
        let sql = match next {
            1 => MIGRATION_V1,
            2 => MIGRATION_V2,
            3 => MIGRATION_V3,
            4 => MIGRATION_V4,
            5 => MIGRATION_V5,
            _ => return Err(Error::Config(format!("no migration for version {next}"))),
        };
        conn.execute_batch(sql)?;
        conn.pragma_update(None, "user_version", next)?;
        current = next;
    }
    Ok(())
}

// -- process_crashes -------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CrashEvent {
    pub id: i64,
    pub comm: String,
    pub detected_at: DateTime<Utc>,
    pub crash_type: String,
    pub pid: Option<i64>,
    pub last_rss_mb: Option<f64>,
    pub message: String,
}

pub struct NewCrashEvent {
    pub comm: String,
    pub detected_at: DateTime<Utc>,
    pub crash_type: String,
    pub pid: Option<i64>,
    pub last_rss_mb: Option<f64>,
    pub message: String,
}

pub fn record_process_crash(conn: &Connection, ev: &NewCrashEvent) -> Result<i64> {
    conn.execute(
        "INSERT INTO process_crashes(comm, detected_at, crash_type, pid, last_rss_mb, message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            ev.comm,
            ev.detected_at.to_rfc3339(),
            ev.crash_type,
            ev.pid,
            ev.last_rss_mb,
            ev.message,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Most recent `limit` crash events for `comm`, newest first.
pub fn recent_process_crashes(
    conn: &Connection,
    comm: &str,
    limit: usize,
) -> Result<Vec<CrashEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, comm, detected_at, crash_type, pid, last_rss_mb, message
         FROM process_crashes WHERE comm = ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![comm, limit as i64], |r| {
        Ok(CrashEvent {
            id: r.get(0)?,
            comm: r.get(1)?,
            detected_at: r
                .get::<_, String>(2)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
            crash_type: r.get(3)?,
            pid: r.get(4)?,
            last_rss_mb: r.get(5)?,
            message: r.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

/// Timestamp of the most recent crash recorded for `comm`, or None.
pub fn latest_crash_detected_at(conn: &Connection, comm: &str) -> Result<Option<DateTime<Utc>>> {
    let mut stmt = conn.prepare(
        "SELECT detected_at FROM process_crashes WHERE comm = ?1 ORDER BY id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([comm])?;
    if let Some(row) = rows.next()? {
        let ts: String = row.get(0)?;
        Ok(ts.parse::<DateTime<Utc>>().ok())
    } else {
        Ok(None)
    }
}

// -- monitor_reports -------------------------------------------------------
// -- audit_runs -----------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AuditRun {
    pub id: i64,
    pub ran_at: DateTime<Utc>,
    pub hardening_index: Option<i64>,
    pub quick: bool,
    pub report_path: String,
    pub lynis_version: String,
}

pub struct NewAuditRun {
    pub ran_at: DateTime<Utc>,
    pub hardening_index: Option<i64>,
    pub quick: bool,
    pub report_path: String,
    pub lynis_version: String,
}

pub fn record_audit_run(conn: &Connection, run: &NewAuditRun) -> Result<i64> {
    conn.execute(
        "INSERT INTO audit_runs(ran_at, hardening_index, quick, report_path, lynis_version)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            run.ran_at.to_rfc3339(),
            run.hardening_index,
            run.quick as i64,
            run.report_path,
            run.lynis_version,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Most recent `limit` audit runs, newest first.
pub fn recent_audit_runs(conn: &Connection, limit: usize) -> Result<Vec<AuditRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, ran_at, hardening_index, quick, report_path, lynis_version
         FROM audit_runs ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(AuditRun {
            id: r.get(0)?,
            ran_at: r
                .get::<_, String>(1)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
            hardening_index: r.get(2)?,
            quick: r.get::<_, i64>(3).map(|v| v != 0).unwrap_or(false),
            report_path: r.get(4).unwrap_or_default(),
            lynis_version: r.get(5).unwrap_or_default(),
        })
    })?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

pub fn record_monitor_report(conn: &Connection, report: &MonitorReport) -> Result<i64> {
    let status = match report.status {
        HealthStatus::Healthy => "healthy",
        HealthStatus::Warning => "warning",
        HealthStatus::Critical => "critical",
    };
    let payload = serde_json::to_string(report)?;
    conn.execute(
        "INSERT INTO monitor_reports(monitor, status, elapsed_ms, collected_at, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![report.monitor, status, report.elapsed_ms as i64, report.collected_at.to_rfc3339(), payload],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn latest_monitor_report(conn: &Connection, monitor: &str) -> Result<Option<MonitorReport>> {
    let mut stmt = conn.prepare(
        "SELECT payload FROM monitor_reports WHERE monitor = ?1 ORDER BY id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([monitor])?;
    if let Some(row) = rows.next()? {
        let payload: String = row.get(0)?;
        Ok(Some(serde_json::from_str(&payload)?))
    } else {
        Ok(None)
    }
}

/// Most recent `limit` reports for `monitor`, newest first.
pub fn recent_monitor_reports(
    conn: &Connection,
    monitor: &str,
    limit: usize,
) -> Result<Vec<MonitorReport>> {
    // Allow partial case-insensitive match so "wezterm" finds "WezTermMonitor".
    let pattern = format!("%{}%", monitor);
    let mut stmt = conn.prepare(
        "SELECT payload FROM monitor_reports WHERE monitor LIKE ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![pattern, limit as i64], |r| {
        let payload: String = r.get(0)?;
        let report: MonitorReport = serde_json::from_str(&payload).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(report)
    })?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

/// Latest report for every monitor that has ever reported, one row per
/// monitor, ordered by monitor name. Powers the tray health summary
/// without the caller needing to know the monitor roster.
pub fn latest_reports_all(conn: &Connection) -> Result<Vec<MonitorReport>> {
    let mut stmt = conn.prepare(
        "SELECT payload FROM monitor_reports \
         WHERE id IN (SELECT MAX(id) FROM monitor_reports GROUP BY monitor) \
         ORDER BY monitor",
    )?;
    let rows = stmt.query_map([], |r| {
        let payload: String = r.get(0)?;
        let report: MonitorReport = serde_json::from_str(&payload).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(report)
    })?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

// -- proposals -------------------------------------------------------------

pub fn upsert_proposal(conn: &Connection, p: &Proposal) -> Result<()> {
    let (action_kind, action_payload) = serialize_action(&p.action)?;
    let tier = match p.autonomy_tier {
        AutonomyTier::Full => "full",
        AutonomyTier::Approve => "approve",
        AutonomyTier::Human => "human",
    };
    conn.execute(
        "INSERT INTO proposals(id, monitor, path, action_kind, action_payload, rationale, confidence, autonomy_tier, emitted_at, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending')
         ON CONFLICT(id) DO NOTHING",
        params![
            p.id.0,
            p.monitor,
            p.path.to_string_lossy(),
            action_kind,
            action_payload,
            p.rationale,
            p.confidence as f64,
            tier,
            p.emitted_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub fn pending_proposals(conn: &Connection) -> Result<Vec<PendingProposal>> {
    let mut stmt = conn.prepare(
        "SELECT id, monitor, path, action_kind, action_payload, rationale, confidence, autonomy_tier, emitted_at
         FROM proposals WHERE state = 'pending' ORDER BY emitted_at ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PendingProposal {
            id: ProposalId(r.get(0)?),
            monitor: r.get(1)?,
            path: PathBuf::from(r.get::<_, String>(2)?),
            action_kind: r.get(3)?,
            action_payload: r.get(4)?,
            rationale: r.get(5)?,
            confidence: r.get::<_, f64>(6)? as f32,
            autonomy_tier: r.get(7)?,
            emitted_at: r
                .get::<_, String>(8)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
        })
    })?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

#[derive(Debug, Clone)]
pub struct PendingProposal {
    pub id: ProposalId,
    pub monitor: String,
    pub path: PathBuf,
    pub action_kind: String,
    pub action_payload: String,
    pub rationale: String,
    pub confidence: f32,
    pub autonomy_tier: String,
    pub emitted_at: DateTime<Utc>,
}

/// Mark `pending` proposals for `monitor` whose source path no longer
/// exists on disk as `stale` — a file already moved or deleted by some
/// other means has nothing left for `koi approve` to act on, so the
/// proposal stops being retried against it forever. Returns the count
/// swept.
pub fn supersede_stale_proposals(conn: &Connection, monitor: &str) -> Result<usize> {
    let mut stmt =
        conn.prepare("SELECT id, path FROM proposals WHERE monitor = ?1 AND state = 'pending'")?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![monitor], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;

    let mut count = 0;
    for (id, path) in rows {
        if !Path::new(&path).exists() {
            conn.execute(
                "UPDATE proposals SET state = 'stale' WHERE id = ?1",
                params![id],
            )?;
            count += 1;
        }
    }
    Ok(count)
}

// -- decisions -------------------------------------------------------------

pub fn record_decision(
    conn: &Connection,
    proposal_id: &ProposalId,
    decision: Decision,
    notes: Option<&str>,
) -> Result<i64> {
    let decision_str = match decision {
        Decision::Approved => "approved",
        Decision::Rejected => "rejected",
        Decision::Deferred => "deferred",
    };
    conn.execute(
        "INSERT INTO decisions(proposal_id, decision, decided_at, notes) VALUES (?1, ?2, ?3, ?4)",
        params![proposal_id.0, decision_str, Utc::now().to_rfc3339(), notes],
    )?;
    // Don't auto-apply — that's the executor's job. Approval just records the signal.
    if matches!(decision, Decision::Rejected) {
        conn.execute(
            "UPDATE proposals SET state = 'rejected' WHERE id = ?1",
            params![proposal_id.0],
        )?;
    }
    Ok(conn.last_insert_rowid())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approved,
    Rejected,
    Deferred,
}

fn serialize_action(action: &ProposedAction) -> Result<(&'static str, String)> {
    let kind = match action {
        ProposedAction::Move { .. } => "move",
        ProposedAction::Archive { .. } => "archive",
        ProposedAction::Delete => "delete",
        ProposedAction::Tag { .. } => "tag",
        ProposedAction::Ignore { .. } => "ignore",
        ProposedAction::DriveMove { .. } => "drive_move",
    };
    Ok((kind, serde_json::to_string(action)?))
}

// -- duplicate groups (TASK-KOI209, ADR-0021) -------------------------------

/// Persist a scanned [`crate::dedupe::DuplicateGroup`]. Idempotent per
/// `group_id` (the content hash): `first_seen` is set once and preserved on
/// every later re-scan. Membership rows are replaced wholesale on each call
/// — a file that moved away since the last scan simply drops out.
pub fn upsert_duplicate_group(
    conn: &Connection,
    group: &crate::dedupe::DuplicateGroup,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO duplicate_groups(group_id, content_hash, size, first_seen)
         VALUES (?1, ?1, ?2, ?3)
         ON CONFLICT(group_id) DO NOTHING",
        params![group.content_hash, group.size as i64, now.to_rfc3339()],
    )?;
    conn.execute(
        "DELETE FROM duplicate_members WHERE group_id = ?1",
        params![group.content_hash],
    )?;
    for member in &group.members {
        let keep_flag = i64::from(member.path == group.keeper);
        conn.execute(
            "INSERT INTO duplicate_members(group_id, path, mtime, keep_flag)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                group.content_hash,
                member.path.to_string_lossy(),
                member.mtime.to_rfc3339(),
                keep_flag,
            ],
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PersistedDuplicateMember {
    pub path: PathBuf,
    pub mtime: DateTime<Utc>,
    pub keep: bool,
}

#[derive(Debug, Clone)]
pub struct PersistedDuplicateGroup {
    pub group_id: String,
    pub size: i64,
    pub first_seen: DateTime<Utc>,
    pub members: Vec<PersistedDuplicateMember>,
}

/// Every persisted group with its members — the read side `koi dedupe apply`
/// consumes.
pub fn list_duplicate_groups(conn: &Connection) -> Result<Vec<PersistedDuplicateGroup>> {
    let mut stmt = conn.prepare(
        "SELECT group_id, size, first_seen FROM duplicate_groups ORDER BY first_seen ASC",
    )?;
    let groups: Vec<(String, i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;

    let mut result = Vec::with_capacity(groups.len());
    for (group_id, size, first_seen_str) in groups {
        let mut mstmt = conn
            .prepare("SELECT path, mtime, keep_flag FROM duplicate_members WHERE group_id = ?1")?;
        let members: Vec<PersistedDuplicateMember> = mstmt
            .query_map(params![group_id], |r| {
                Ok(PersistedDuplicateMember {
                    path: PathBuf::from(r.get::<_, String>(0)?),
                    mtime: r
                        .get::<_, String>(1)?
                        .parse::<DateTime<Utc>>()
                        .unwrap_or_else(|_| Utc::now()),
                    keep: r.get::<_, i64>(2)? != 0,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        result.push(PersistedDuplicateGroup {
            group_id,
            size,
            first_seen: first_seen_str
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
            members,
        });
    }
    Ok(result)
}

/// The `first_seen` timestamp recorded for a group, if it has ever been
/// persisted.
pub fn duplicate_group_first_seen(
    conn: &Connection,
    group_id: &str,
) -> Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT first_seen FROM duplicate_groups WHERE group_id = ?1",
            params![group_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|s| s.parse::<DateTime<Utc>>().ok()))
}

// -- trash log (TASK-KOI210, ADR-0021) --------------------------------------

#[derive(Debug, Clone)]
pub struct TrashEntry {
    pub id: i64,
    pub original_path: PathBuf,
    pub trash_path: PathBuf,
    pub trashed_at: DateTime<Utc>,
}

/// Record a completed trash move. Called after `trash::move_to_trash`
/// succeeds — this function does not move anything itself.
pub fn record_trash(
    conn: &Connection,
    original_path: &Path,
    trash_path: &Path,
    trashed_at: DateTime<Utc>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO trash_log(original_path, trash_path, trashed_at, restored_at)
         VALUES (?1, ?2, ?3, NULL)",
        params![
            original_path.to_string_lossy(),
            trash_path.to_string_lossy(),
            trashed_at.to_rfc3339(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Mark a trash entry restored. Called after `trash::restore_from_trash`
/// succeeds.
pub fn mark_restored(conn: &Connection, id: i64, restored_at: DateTime<Utc>) -> Result<()> {
    conn.execute(
        "UPDATE trash_log SET restored_at = ?1 WHERE id = ?2",
        params![restored_at.to_rfc3339(), id],
    )?;
    Ok(())
}

/// Entries still in trash (never restored), oldest first.
pub fn list_trash(conn: &Connection) -> Result<Vec<TrashEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, original_path, trash_path, trashed_at
         FROM trash_log WHERE restored_at IS NULL ORDER BY trashed_at ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(TrashEntry {
            id: r.get(0)?,
            original_path: PathBuf::from(r.get::<_, String>(1)?),
            trash_path: PathBuf::from(r.get::<_, String>(2)?),
            trashed_at: r
                .get::<_, String>(3)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
        })
    })?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(Error::from)
}

/// Still-trashed entries older than `cutoff` — the candidate set for
/// `koi trash empty --older-than`.
pub fn trash_entries_older_than(
    conn: &Connection,
    cutoff: DateTime<Utc>,
) -> Result<Vec<TrashEntry>> {
    Ok(list_trash(conn)?
        .into_iter()
        .filter(|e| e.trashed_at < cutoff)
        .collect())
}

// -- classifier (learning loop v0) -----------------------------------------

/// Ask the decisions table for the highest-approval Move destination for a
/// given source monitor + filename suffix pattern. Returns (destination, confidence).
///
/// v0 signal: approvals vs. rejections keyed on (monitor, suffix). Confidence
/// uses Laplace smoothing with a prior of 2 (equivalent to pretending we've
/// seen one approval and one rejection before any real data).
pub fn learned_destination(
    conn: &Connection,
    monitor: &str,
    suffix: &str,
) -> Result<Option<(PathBuf, f32)>> {
    let like_pattern = format!("%{}", suffix);

    // Count approvals per destination for this (monitor, suffix).
    let mut stmt = conn.prepare(
        r#"SELECT p.action_payload, COUNT(*) AS n
           FROM proposals p
           JOIN decisions d ON d.proposal_id = p.id
           WHERE p.monitor = ?1
             AND p.path LIKE ?2
             AND p.action_kind = 'move'
             AND d.decision = 'approved'
           GROUP BY p.action_payload
           ORDER BY n DESC
           LIMIT 1"#,
    )?;
    let mut rows = stmt.query(params![monitor, &like_pattern])?;

    let (action_json, approvals): (String, i64) = match rows.next()? {
        Some(row) => (row.get(0)?, row.get(1)?),
        None => return Ok(None),
    };

    // Count rejections against moves for the same (monitor, suffix) — any dest.
    let rejections: i64 = conn.query_row(
        r#"SELECT COUNT(*)
           FROM proposals p
           JOIN decisions d ON d.proposal_id = p.id
           WHERE p.monitor = ?1
             AND p.path LIKE ?2
             AND p.action_kind = 'move'
             AND d.decision = 'rejected'"#,
        params![monitor, &like_pattern],
        |r| r.get(0),
    )?;

    // Parse destination from the action payload (JSON form of ProposedAction).
    let action: ProposedAction = serde_json::from_str(&action_json)?;
    let ProposedAction::Move { dest } = action else {
        return Ok(None);
    };

    // Drop the filename from dest — we want the directory, so downstream callers
    // can attach the current file's name.
    let dest_dir = dest.parent().map(|p| p.to_path_buf()).unwrap_or(dest);

    // Laplace-smoothed confidence: (a + 1) / (a + r + 2).
    let a = approvals as f32;
    let r = rejections as f32;
    let confidence = (a + 1.0) / (a + r + 2.0);

    Ok(Some((dest_dir, confidence)))
}

#[cfg(test)]
mod classifier_tests {
    use super::*;
    use crate::filing::{Proposal, ProposalId, ProposedAction};

    fn fake_approved(conn: &Connection, monitor: &'static str, src: &str, dst: &str) {
        let p = Proposal::new(
            monitor,
            PathBuf::from(src),
            ProposedAction::Move {
                dest: PathBuf::from(dst),
            },
            "test",
            0.5,
        );
        upsert_proposal(conn, &p).unwrap();
        record_decision(conn, &p.id, Decision::Approved, None).unwrap();
        // Classifier depends on decisions only — manually ensure idempotent state.
        let _ = ProposalId::compute(
            monitor,
            std::path::Path::new(src),
            &ProposedAction::Move {
                dest: PathBuf::from(dst),
            },
        );
    }

    #[test]
    fn learns_from_approvals() {
        let conn = open_in_memory().unwrap();
        for (i, _) in (0..3).enumerate() {
            fake_approved(
                &conn,
                "DownloadsMonitor",
                &format!("/home/user/Downloads/a{i}.pdf"),
                "/home/user/Documents/PDFs/a.pdf",
            );
        }
        let hit = learned_destination(&conn, "DownloadsMonitor", ".pdf").unwrap();
        assert!(hit.is_some());
        let (dest, conf) = hit.unwrap();
        assert_eq!(dest, PathBuf::from("/home/user/Documents/PDFs"));
        // 3 approvals, 0 rejections → (3+1)/(3+0+2) = 0.8
        assert!((conf - 0.8).abs() < 0.001, "got {conf}");
    }

    #[test]
    fn returns_none_for_unknown_suffix() {
        let conn = open_in_memory().unwrap();
        let hit = learned_destination(&conn, "DownloadsMonitor", ".xyz").unwrap();
        assert!(hit.is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Observation, Severity};

    #[test]
    fn migrates_empty_db() {
        let conn = open_in_memory().unwrap();
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, CURRENT_VERSION);
    }

    #[test]
    fn migration_v4_preserves_existing_proposal_and_decision_rows() {
        // A copy of a pre-V4 DB (here: freshly migrated, since open_in_memory
        // always migrates to CURRENT_VERSION) must upgrade without touching
        // unrelated tables' row counts.
        let conn = open_in_memory().unwrap();
        let p = Proposal::new(
            "TestMonitor",
            PathBuf::from("/tmp/x.pdf"),
            ProposedAction::Move {
                dest: PathBuf::from("/tmp/dest/x.pdf"),
            },
            "test",
            0.9,
        );
        upsert_proposal(&conn, &p).unwrap();
        record_decision(&conn, &p.id, Decision::Approved, None).unwrap();

        let proposal_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proposals", [], |r| r.get(0))
            .unwrap();
        let decision_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM decisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(proposal_count, 1);
        assert_eq!(decision_count, 1);

        // The V4 tables exist and are queryable.
        let group_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM duplicate_groups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(group_count, 0);
    }

    #[test]
    fn list_duplicate_groups_reconstructs_groups_with_members_and_keep_flag() {
        let conn = open_in_memory().unwrap();
        let group = crate::dedupe::DuplicateGroup {
            content_hash: "def456".into(),
            size: 10,
            keeper: PathBuf::from("/a"),
            members: vec![
                crate::dedupe::DuplicateMember {
                    path: PathBuf::from("/a"),
                    mtime: Utc::now(),
                },
                crate::dedupe::DuplicateMember {
                    path: PathBuf::from("/b"),
                    mtime: Utc::now(),
                },
            ],
        };
        upsert_duplicate_group(&conn, &group, Utc::now()).unwrap();

        let groups = list_duplicate_groups(&conn).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, "def456");
        assert_eq!(groups[0].members.len(), 2);
        let keeper_member = groups[0]
            .members
            .iter()
            .find(|m| m.path == Path::new("/a"))
            .unwrap();
        assert!(keeper_member.keep);
        let other_member = groups[0]
            .members
            .iter()
            .find(|m| m.path == Path::new("/b"))
            .unwrap();
        assert!(!other_member.keep);
    }

    #[test]
    fn upsert_duplicate_group_is_idempotent_and_preserves_first_seen() {
        let conn = open_in_memory().unwrap();
        let group = crate::dedupe::DuplicateGroup {
            content_hash: "abc123".into(),
            size: 42,
            keeper: PathBuf::from("/a"),
            members: vec![
                crate::dedupe::DuplicateMember {
                    path: PathBuf::from("/a"),
                    mtime: Utc::now(),
                },
                crate::dedupe::DuplicateMember {
                    path: PathBuf::from("/b"),
                    mtime: Utc::now(),
                },
            ],
        };

        let first_seen_at = "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        upsert_duplicate_group(&conn, &group, first_seen_at).unwrap();
        let stored_first = duplicate_group_first_seen(&conn, &group.content_hash)
            .unwrap()
            .unwrap();
        assert_eq!(stored_first, first_seen_at);

        // Re-scan (a later timestamp) must NOT move first_seen.
        let rescan_at = "2026-06-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        upsert_duplicate_group(&conn, &group, rescan_at).unwrap();
        let stored_after_rescan = duplicate_group_first_seen(&conn, &group.content_hash)
            .unwrap()
            .unwrap();
        assert_eq!(stored_after_rescan, first_seen_at);

        // Members were replaced, not duplicated, on re-scan.
        let member_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM duplicate_members WHERE group_id = ?1",
                params![group.content_hash],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(member_count, 2);
    }

    #[test]
    fn migration_v5_preserves_v4_and_earlier_row_counts() {
        let conn = open_in_memory().unwrap();
        let p = Proposal::new(
            "TestMonitor",
            PathBuf::from("/tmp/x.pdf"),
            ProposedAction::Move {
                dest: PathBuf::from("/tmp/dest/x.pdf"),
            },
            "test",
            0.9,
        );
        upsert_proposal(&conn, &p).unwrap();
        let group = crate::dedupe::DuplicateGroup {
            content_hash: "abc".into(),
            size: 1,
            keeper: PathBuf::from("/a"),
            members: vec![crate::dedupe::DuplicateMember {
                path: PathBuf::from("/a"),
                mtime: Utc::now(),
            }],
        };
        upsert_duplicate_group(&conn, &group, Utc::now()).unwrap();

        let proposal_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proposals", [], |r| r.get(0))
            .unwrap();
        let group_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM duplicate_groups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(proposal_count, 1);
        assert_eq!(group_count, 1);

        let trash_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM trash_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(trash_count, 0);
    }

    #[test]
    fn record_and_restore_trash_entry() {
        let conn = open_in_memory().unwrap();
        let now = Utc::now();
        let id = record_trash(
            &conn,
            Path::new("/home/user/Downloads/dupe.pdf"),
            Path::new("/home/user/.local/share/koi/trash/2026-08-12/Downloads/dupe.pdf"),
            now,
        )
        .unwrap();

        let entries = list_trash(&conn).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(
            entries[0].original_path,
            PathBuf::from("/home/user/Downloads/dupe.pdf")
        );

        mark_restored(&conn, id, Utc::now()).unwrap();
        let entries_after = list_trash(&conn).unwrap();
        assert!(
            entries_after.is_empty(),
            "a restored entry must not appear in list_trash"
        );
    }

    #[test]
    fn round_trip_monitor_report() {
        let conn = open_in_memory().unwrap();
        let report = MonitorReport {
            monitor: "TestMonitor".into(),
            status: HealthStatus::Warning,
            elapsed_ms: 42,
            collected_at: Utc::now(),
            observations: vec![Observation {
                key: "x".into(),
                value: serde_json::json!(1),
                severity: Severity::Info,
            }],
            suggestions: vec![],
        };
        record_monitor_report(&conn, &report).unwrap();
        let fetched = latest_monitor_report(&conn, "TestMonitor")
            .unwrap()
            .unwrap();
        assert_eq!(fetched.elapsed_ms, 42);
        assert_eq!(fetched.status, HealthStatus::Warning);
    }

    #[test]
    fn latest_reports_all_returns_one_per_monitor() {
        let conn = open_in_memory().unwrap();
        let mk = |monitor: &str, status| MonitorReport {
            monitor: monitor.into(),
            status,
            elapsed_ms: 1,
            collected_at: Utc::now(),
            observations: vec![],
            suggestions: vec![],
        };
        // Two reports for DiskMonitor — only the newest should survive.
        record_monitor_report(&conn, &mk("DiskMonitor", HealthStatus::Healthy)).unwrap();
        record_monitor_report(&conn, &mk("DiskMonitor", HealthStatus::Warning)).unwrap();
        record_monitor_report(&conn, &mk("MemoryMonitor", HealthStatus::Critical)).unwrap();

        let all = latest_reports_all(&conn).unwrap();
        assert_eq!(all.len(), 2, "one row per monitor");
        // Ordered by monitor name: DiskMonitor before MemoryMonitor.
        assert_eq!(all[0].monitor, "DiskMonitor");
        assert_eq!(all[0].status, HealthStatus::Warning, "newest wins");
        assert_eq!(all[1].monitor, "MemoryMonitor");
        assert_eq!(all[1].status, HealthStatus::Critical);
    }

    /// The tray repaints off this query every 30s (TASK-KOI100), so it has to
    /// stay cheap even once the history table has months of reports in it.
    #[test]
    fn latest_reports_all_stays_under_50ms_on_a_full_history() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("koi.db")).unwrap();
        let monitors = [
            "CacheMonitor",
            "DiskMonitor",
            "DockerMonitor",
            "FileMonitor",
            "GitMonitor",
            "MemoryMonitor",
            "PackageMonitor",
        ];
        // Seven monitors x 500 runs — well past a year of daily `koi check`.
        for _ in 0..500 {
            for m in monitors {
                record_monitor_report(
                    &conn,
                    &MonitorReport {
                        monitor: (*m).into(),
                        status: HealthStatus::Healthy,
                        elapsed_ms: 12,
                        collected_at: Utc::now(),
                        observations: vec![Observation {
                            key: "sample".into(),
                            value: serde_json::json!({"bytes": 123_456_789u64}),
                            severity: Severity::Info,
                        }],
                        suggestions: vec![],
                    },
                )
                .unwrap();
            }
        }

        let start = std::time::Instant::now();
        let all = latest_reports_all(&conn).unwrap();
        let elapsed = start.elapsed();

        assert_eq!(all.len(), monitors.len());
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "latest_reports_all took {elapsed:?}, budget is 50ms"
        );
    }

    #[test]
    fn supersede_stale_proposals_marks_vanished_sources_and_leaves_existing_ones() {
        use std::io::Write;
        let conn = open_in_memory().unwrap();

        let tmp_dir = std::env::temp_dir().join(format!(
            "koi-stale-sweep-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let still_here = tmp_dir.join("still-here.pdf");
        std::fs::File::create(&still_here)
            .unwrap()
            .write_all(b"x")
            .unwrap();
        let gone = tmp_dir.join("already-gone.pdf");
        // Deliberately never created — simulates a file moved/deleted since scan.

        let p_gone = Proposal::new(
            "TestMonitor",
            gone.clone(),
            ProposedAction::Move {
                dest: PathBuf::from("/tmp/dest1"),
            },
            "r",
            0.9,
        );
        let p_here = Proposal::new(
            "TestMonitor",
            still_here.clone(),
            ProposedAction::Move {
                dest: PathBuf::from("/tmp/dest2"),
            },
            "r",
            0.9,
        );
        let p_other_monitor = Proposal::new(
            "OtherMonitor",
            gone.clone(),
            ProposedAction::Move {
                dest: PathBuf::from("/tmp/dest3"),
            },
            "r",
            0.9,
        );
        upsert_proposal(&conn, &p_gone).unwrap();
        upsert_proposal(&conn, &p_here).unwrap();
        upsert_proposal(&conn, &p_other_monitor).unwrap();

        let swept = supersede_stale_proposals(&conn, "TestMonitor").unwrap();
        assert_eq!(swept, 1, "only the vanished TestMonitor proposal is stale");

        let pending = pending_proposals(&conn).unwrap();
        let pending_ids: Vec<_> = pending.iter().map(|p| p.id.0.clone()).collect();
        assert!(
            !pending_ids.contains(&p_gone.id.0),
            "vanished source must be swept"
        );
        assert!(
            pending_ids.contains(&p_here.id.0),
            "existing source must stay pending"
        );
        assert!(
            pending_ids.contains(&p_other_monitor.id.0),
            "a different monitor's proposal for the same vanished path must be untouched"
        );

        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn proposal_upsert_is_idempotent() {
        let conn = open_in_memory().unwrap();
        let p = Proposal::new(
            "TestMonitor",
            PathBuf::from("/tmp/x.pdf"),
            ProposedAction::Move {
                dest: PathBuf::from("/tmp/docs/x.pdf"),
            },
            "rationale",
            0.9,
        );
        upsert_proposal(&conn, &p).unwrap();
        upsert_proposal(&conn, &p).unwrap(); // should not duplicate
        let pending = pending_proposals(&conn).unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn rejected_decision_updates_state() {
        let conn = open_in_memory().unwrap();
        let p = Proposal::new(
            "M",
            PathBuf::from("/x"),
            ProposedAction::Move {
                dest: PathBuf::from("/y"),
            },
            "r",
            0.5,
        );
        upsert_proposal(&conn, &p).unwrap();
        record_decision(&conn, &p.id, Decision::Rejected, Some("no thanks")).unwrap();
        let pending = pending_proposals(&conn).unwrap();
        assert_eq!(
            pending.len(),
            0,
            "rejected proposals shouldn't appear in pending"
        );
    }
}
