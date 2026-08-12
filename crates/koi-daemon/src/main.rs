//! koi-daemon — long-running tokio service.
//!
//! Current scope:
//! - Load the six health monitors.
//! - Run each on its own cadence via tokio::time::interval.
//! - Persist each report to the SQLite state DB.
//! - Run DownloadsMonitor daily, persisting proposals.
//! - Shutdown cleanly on SIGINT/SIGTERM.
//!
//! This is a skeleton; policy tuning (backoff on failure, monitor-level opt-in,
//! CEL rules) arrives with later stories.

use anyhow::{Context, Result};
use koi_core::{
    config::FilingConfig,
    filing::{
        DocumentsMonitor, DownloadsMonitor, FileMonitor, InboxMonitor, ScanContext,
        SqliteClassifier,
    },
    monitors::{
        CacheMonitor, DiskMonitor, DockerMonitor, GitMonitor, LatencyMonitor, MemoryMonitor,
        NetworkMonitor, PackageMonitor,
    },
    state, Monitor,
};
use rusqlite::Connection;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{signal, task::JoinSet};
use tracing::{error, info, warn};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let _log_guard = init_tracing();
    info!("koi-daemon starting");

    let db_path = state::default_db_path().context("resolve DB path")?;
    let conn = state::open(&db_path).context("open SQLite state")?;
    let db: Arc<Mutex<Connection>> = Arc::new(Mutex::new(conn));
    info!("state ready at {}", db_path.display());

    let filing_cfg = FilingConfig::load();
    info!(
        downloads_hours = filing_cfg.cadences.downloads_hours,
        documents_hours = filing_cfg.cadences.documents_hours,
        inbox_hours = filing_cfg.cadences.inbox_hours,
        "filing config loaded"
    );

    let mut tasks = JoinSet::new();

    // Health monitors — each gets its own cadence.
    spawn_health_loop(
        &mut tasks,
        "DiskMonitor",
        Duration::from_secs(6 * 3600),
        db.clone(),
        Box::new(DiskMonitor::new().context("DiskMonitor init")?) as Box<dyn Monitor + Send + Sync>,
    );
    spawn_health_loop(
        &mut tasks,
        "MemoryMonitor",
        Duration::from_secs(60),
        db.clone(),
        Box::new(MemoryMonitor::new()),
    );
    spawn_health_loop(
        &mut tasks,
        "CacheMonitor",
        Duration::from_secs(6 * 3600),
        db.clone(),
        Box::new(CacheMonitor::new().context("CacheMonitor init")?),
    );
    spawn_health_loop(
        &mut tasks,
        "DockerMonitor",
        Duration::from_secs(30 * 60),
        db.clone(),
        Box::new(DockerMonitor::new()),
    );
    spawn_health_loop(
        &mut tasks,
        "GitMonitor",
        Duration::from_secs(15 * 60),
        db.clone(),
        Box::new(GitMonitor::new().context("GitMonitor init")?),
    );
    spawn_health_loop(
        &mut tasks,
        "PackageMonitor",
        Duration::from_secs(24 * 3600),
        db.clone(),
        Box::new(PackageMonitor::new().context("PackageMonitor init")?),
    );
    spawn_health_loop(
        &mut tasks,
        "NetworkMonitor",
        Duration::from_secs(5 * 60),
        db.clone(),
        Box::new(NetworkMonitor::new()),
    );
    spawn_health_loop(
        &mut tasks,
        "LatencyMonitor",
        Duration::from_secs(2 * 60),
        db.clone(),
        Box::new(LatencyMonitor::new()),
    );

    // File monitors — cadence from filing.toml (per ADR-0014 defaults when absent).
    spawn_scan_loop(
        &mut tasks,
        "DownloadsMonitor",
        Duration::from_secs(filing_cfg.cadences.downloads_hours * 3600),
        db.clone(),
        db_path.clone(),
        Box::new(DownloadsMonitor::from_config(&filing_cfg).context("DownloadsMonitor init")?),
    );
    spawn_scan_loop(
        &mut tasks,
        "DocumentsMonitor",
        Duration::from_secs(filing_cfg.cadences.documents_hours * 3600),
        db.clone(),
        db_path.clone(),
        Box::new(DocumentsMonitor::from_config(&filing_cfg).context("DocumentsMonitor init")?),
    );
    spawn_scan_loop(
        &mut tasks,
        "InboxMonitor",
        Duration::from_secs(filing_cfg.cadences.inbox_hours * 3600),
        db.clone(),
        db_path.clone(),
        Box::new(InboxMonitor::from_config(&filing_cfg).context("InboxMonitor init")?),
    );

    // Unattended duplicate-group scan (FEAT-KOI054 AC-7). Read-only against
    // the filesystem — never trashes anything; `koi dedupe apply`/`koi trash`
    // stay human-initiated CLI-only.
    spawn_dedupe_scan_loop(
        &mut tasks,
        Duration::from_secs(filing_cfg.dedupe.scan_interval_days * 24 * 3600),
        db.clone(),
        dedupe_roots(&filing_cfg)?,
        filing_cfg.dedupe.max_size_mb * 1024 * 1024,
    );

    info!("{} loop(s) spawned — awaiting SIGINT/SIGTERM", tasks.len());

    tokio::select! {
        _ = signal::ctrl_c() => info!("SIGINT received"),
        _ = sigterm() => info!("SIGTERM received"),
    }

    info!("shutting down — aborting loops");
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    info!("koi-daemon stopped");
    Ok(())
}

fn spawn_health_loop(
    tasks: &mut JoinSet<()>,
    name: &'static str,
    cadence: Duration,
    db: Arc<Mutex<Connection>>,
    monitor: Box<dyn Monitor + Send + Sync>,
) {
    tasks.spawn(async move {
        let monitor = Arc::new(monitor);
        // First tick fires immediately.
        let mut ticker = tokio::time::interval(cadence);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let monitor = monitor.clone();
            let db = db.clone();
            let res = tokio::task::spawn_blocking(move || -> Result<()> {
                let report = monitor.run().context("monitor run")?;
                let conn = db
                    .lock()
                    .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
                state::record_monitor_report(&conn, &report).context("record report")?;
                Ok(())
            })
            .await;
            match res {
                Ok(Ok(())) => info!(name, "tick ok"),
                Ok(Err(e)) => warn!(name, error = %e, "monitor tick failed"),
                Err(e) => error!(name, error = %e, "monitor task panicked"),
            }
        }
    });
}

fn spawn_scan_loop(
    tasks: &mut JoinSet<()>,
    name: &'static str,
    cadence: Duration,
    db: Arc<Mutex<Connection>>,
    db_path: std::path::PathBuf,
    monitor: Box<dyn FileMonitor + Send + Sync>,
) {
    tasks.spawn(async move {
        let monitor = Arc::new(monitor);
        let mut ticker = tokio::time::interval(cadence);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let monitor = monitor.clone();
            let db = db.clone();
            let db_path = db_path.clone();
            let res = tokio::task::spawn_blocking(move || -> Result<usize> {
                // Open a separate connection for the classifier (read-only use).
                let classifier_conn = state::open(&db_path).context("classifier conn")?;
                let ctx = ScanContext::new_now_with_roots(&monitor.roots())
                    .with_classifier(Box::new(SqliteClassifier::new(classifier_conn)));
                let proposals = monitor.scan(&ctx).context("file scan")?;
                let conn = db
                    .lock()
                    .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
                let count = proposals.len();
                for p in &proposals {
                    state::upsert_proposal(&conn, p).context("upsert proposal")?;
                }
                Ok(count)
            })
            .await;
            match res {
                Ok(Ok(n)) => info!(name, "scan ok, {n} proposal(s) persisted"),
                Ok(Err(e)) => warn!(name, error = %e, "scan tick failed"),
                Err(e) => error!(name, error = %e, "scan task panicked"),
            }
        }
    });
}

/// Default dedupe roots: configured overrides, falling back to `$HOME`'s
/// Downloads/Documents/inbox — the same set `koi dedupe scan` defaults to.
fn dedupe_roots(cfg: &FilingConfig) -> Result<Vec<std::path::PathBuf>> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("$HOME not set"))?;
    Ok(vec![
        cfg.roots
            .downloads
            .clone()
            .unwrap_or_else(|| home.join("Downloads")),
        cfg.roots
            .documents
            .clone()
            .unwrap_or_else(|| home.join("Documents")),
        cfg.roots
            .inbox
            .clone()
            .unwrap_or_else(|| home.join("inbox")),
    ])
}

/// Unattended duplicate-group scan (FEAT-KOI054 AC-7). Read-only against the
/// filesystem — persists groups via `state::upsert_duplicate_group` and
/// never calls anything trash- or delete-shaped; `koi dedupe apply`/
/// `koi trash` stay human-initiated CLI-only.
fn spawn_dedupe_scan_loop(
    tasks: &mut JoinSet<()>,
    cadence: Duration,
    db: Arc<Mutex<Connection>>,
    roots: Vec<std::path::PathBuf>,
    max_size_bytes: u64,
) {
    tasks.spawn(async move {
        let mut ticker = tokio::time::interval(cadence);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let db = db.clone();
            let roots = roots.clone();
            let res = tokio::task::spawn_blocking(move || -> Result<usize> {
                let groups = koi_core::dedupe::scan(&roots, max_size_bytes);
                let conn = db
                    .lock()
                    .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
                let now = chrono::Utc::now();
                for g in &groups {
                    state::upsert_duplicate_group(&conn, g, now)
                        .context("upsert duplicate group")?;
                }
                Ok(groups.len())
            })
            .await;
            match res {
                Ok(Ok(n)) => info!(name = "DedupeScan", "scan ok, {n} group(s) persisted"),
                Ok(Err(e)) => warn!(name = "DedupeScan", error = %e, "dedupe scan tick failed"),
                Err(e) => error!(name = "DedupeScan", error = %e, "dedupe scan task panicked"),
            }
        }
    });
}

#[cfg(unix)]
async fn sigterm() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut stream) => {
            stream.recv().await;
        }
        Err(_) => std::future::pending::<()>().await,
    }
}

#[cfg(not(unix))]
async fn sigterm() {
    std::future::pending::<()>().await;
}

fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let stderr_layer = fmt::layer().with_target(false);

    // File appender with daily rotation in the user data dir.
    let (file_layer, guard) = match log_dir() {
        Some(dir) => {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("warn: could not create log dir {}: {e}", dir.display());
                return None;
            }
            let appender = tracing_appender::rolling::daily(&dir, "koi-daemon.log");
            let (non_blocking, guard) = tracing_appender::non_blocking(appender);
            (
                Some(
                    fmt::layer()
                        .with_writer(non_blocking)
                        .with_ansi(false)
                        .json(),
                ),
                Some(guard),
            )
        }
        None => (None, None),
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer);
    if let Some(fl) = file_layer {
        registry.with(fl).init();
    } else {
        registry.init();
    }
    guard
}

fn log_dir() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("com", "powellclark", "koi").map(|d| d.data_dir().join("logs"))
}
