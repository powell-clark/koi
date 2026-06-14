//! koi-tray — Tauri v2 system tray app: a one-glance health summary and a
//! consent surface for pending file-lifecycle proposals.
//!
//! The tray icon colour reflects overall system health. Clicking the tray opens
//! a popover that reads the latest monitor reports and pending proposals from
//! koi-core's SQLite store and offers per-proposal approve/reject backed by the
//! existing executor.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use koi_core::{
    filing::{self, Outcome, ProposedAction},
    state,
    types::HealthStatus,
};
use serde::Serialize;
use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};
use tracing::{info, warn};

const TRAY_ID: &str = "koi";

// Status icons rendered at build time (icons/status/*.png).
const ICON_GREEN: &[u8] = include_bytes!("../icons/status/green.png");
const ICON_AMBER: &[u8] = include_bytes!("../icons/status/amber.png");
const ICON_RED: &[u8] = include_bytes!("../icons/status/red.png");

// ---- command payloads ----------------------------------------------------

#[derive(Serialize)]
struct MonitorRow {
    monitor: String,
    status: String,
    elapsed_ms: u64,
    collected_at: String,
}

#[derive(Serialize)]
struct HealthSummary {
    overall: String,
    monitors: Vec<MonitorRow>,
    pending_count: usize,
}

#[derive(Serialize)]
struct ProposalRow {
    id: String,
    monitor: String,
    path: String,
    action_kind: String,
    dest: Option<String>,
    rationale: String,
    confidence: f32,
}

// ---- helpers -------------------------------------------------------------

fn open_db() -> Result<rusqlite::Connection, String> {
    let path = state::default_db_path().map_err(|e| e.to_string())?;
    state::open(&path).map_err(|e| e.to_string())
}

fn status_str(s: HealthStatus) -> &'static str {
    match s {
        HealthStatus::Healthy => "healthy",
        HealthStatus::Warning => "warning",
        HealthStatus::Critical => "critical",
    }
}

/// Worst status wins — one critical monitor makes the whole system critical.
fn overall_status(reports: &[koi_core::types::MonitorReport]) -> HealthStatus {
    let mut worst = HealthStatus::Healthy;
    for r in reports {
        worst = match (worst, r.status) {
            (HealthStatus::Critical, _) | (_, HealthStatus::Critical) => HealthStatus::Critical,
            (HealthStatus::Warning, _) | (_, HealthStatus::Warning) => HealthStatus::Warning,
            _ => HealthStatus::Healthy,
        };
    }
    worst
}

fn icon_for(status: HealthStatus) -> tauri::Result<Image<'static>> {
    let bytes = match status {
        HealthStatus::Healthy => ICON_GREEN,
        HealthStatus::Warning => ICON_AMBER,
        HealthStatus::Critical => ICON_RED,
    };
    Image::from_bytes(bytes)
}

/// Recompute overall health and repaint the tray icon to match.
fn refresh_tray_icon(app: &AppHandle) {
    let status =
        match open_db().and_then(|c| state::latest_reports_all(&c).map_err(|e| e.to_string())) {
            Ok(reports) => overall_status(&reports),
            Err(e) => {
                warn!("tray icon refresh: {e}");
                return;
            }
        };
    if let (Some(tray), Ok(icon)) = (app.tray_by_id(TRAY_ID), icon_for(status)) {
        if let Err(e) = tray.set_icon(Some(icon)) {
            warn!("set_icon failed: {e}");
        }
    }
}

fn toggle_popover(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        match win.is_visible() {
            Ok(true) => {
                let _ = win.hide();
            }
            _ => {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }
    }
}

// ---- commands ------------------------------------------------------------

#[tauri::command]
fn health_summary(app: AppHandle) -> Result<HealthSummary, String> {
    let conn = open_db()?;
    let reports = state::latest_reports_all(&conn).map_err(|e| e.to_string())?;
    let overall = overall_status(&reports);
    let pending_count = state::pending_proposals(&conn)
        .map_err(|e| e.to_string())?
        .len();

    // Keep the tray icon in sync whenever the popover refreshes.
    if let (Some(tray), Ok(icon)) = (app.tray_by_id(TRAY_ID), icon_for(overall)) {
        let _ = tray.set_icon(Some(icon));
    }

    let monitors = reports
        .into_iter()
        .map(|r| MonitorRow {
            monitor: r.monitor,
            status: status_str(r.status).to_string(),
            elapsed_ms: r.elapsed_ms,
            collected_at: r.collected_at.to_rfc3339(),
        })
        .collect();

    Ok(HealthSummary {
        overall: status_str(overall).to_string(),
        monitors,
        pending_count,
    })
}

#[tauri::command]
fn list_proposals() -> Result<Vec<ProposalRow>, String> {
    let conn = open_db()?;
    let pending = state::pending_proposals(&conn).map_err(|e| e.to_string())?;
    let rows = pending
        .into_iter()
        .map(|p| {
            let dest = serde_json::from_str::<ProposedAction>(&p.action_payload)
                .ok()
                .and_then(|a| match a {
                    ProposedAction::Move { dest } => Some(dest.display().to_string()),
                    _ => None,
                });
            ProposalRow {
                id: p.id.0,
                monitor: p.monitor,
                path: p.path.display().to_string(),
                action_kind: p.action_kind,
                dest,
                rationale: p.rationale,
                confidence: p.confidence,
            }
        })
        .collect();
    Ok(rows)
}

#[tauri::command]
fn approve_proposal(app: AppHandle, id: String) -> Result<(), String> {
    let conn = open_db()?;
    let pending = state::pending_proposals(&conn).map_err(|e| e.to_string())?;
    let p = pending
        .into_iter()
        .find(|p| p.id.0 == id)
        .ok_or_else(|| "proposal not found".to_string())?;

    let action: ProposedAction =
        serde_json::from_str(&p.action_payload).map_err(|e| e.to_string())?;

    match filing::apply(&p.path, &action) {
        Outcome::Applied => {
            state::record_decision(
                &conn,
                &p.id,
                state::Decision::Approved,
                Some("applied via tray"),
            )
            .map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE proposals SET state = 'applied' WHERE id = ?1",
                rusqlite::params![p.id.0],
            )
            .map_err(|e| e.to_string())?;
            refresh_tray_icon(&app);
            Ok(())
        }
        Outcome::Skipped(why) => Err(format!("skipped: {why}")),
        Outcome::Failed(why) => {
            let _ = conn.execute(
                "UPDATE proposals SET state = 'failed' WHERE id = ?1",
                rusqlite::params![p.id.0],
            );
            Err(format!("failed: {why}"))
        }
    }
}

#[tauri::command]
fn reject_proposal(app: AppHandle, id: String) -> Result<(), String> {
    let conn = open_db()?;
    let pending = state::pending_proposals(&conn).map_err(|e| e.to_string())?;
    let p = pending
        .into_iter()
        .find(|p| p.id.0 == id)
        .ok_or_else(|| "proposal not found".to_string())?;
    state::record_decision(
        &conn,
        &p.id,
        state::Decision::Rejected,
        Some("rejected via tray"),
    )
    .map_err(|e| e.to_string())?;
    refresh_tray_icon(&app);
    Ok(())
}

// ---- app -----------------------------------------------------------------

fn main() {
    tracing_subscriber::fmt().init();

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            health_summary,
            list_proposals,
            approve_proposal,
            reject_proposal
        ])
        .setup(|app| {
            info!("Initializing koi system tray app");

            let menu = MenuBuilder::new(app)
                .item(&MenuItemBuilder::with_id("open", "Open koi").build(app)?)
                .separator()
                .item(&MenuItemBuilder::with_id("quit", "Quit").build(app)?)
                .build()?;

            // Start green; repainted to the real state immediately below.
            TrayIconBuilder::with_id(TRAY_ID)
                .icon(icon_for(HealthStatus::Healthy)?)
                .tooltip("koi — system health")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "open" => toggle_popover(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_popover(tray.app_handle());
                    }
                })
                .build(app)?;

            // Paint the real health colour at startup.
            refresh_tray_icon(app.handle());

            info!("koi system tray initialized");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running koi tray application");
}
