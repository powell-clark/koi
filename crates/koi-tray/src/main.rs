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
    types::{HealthStatus, MonitorReport, Severity},
};
use serde::Serialize;
use std::time::Duration;
use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};
use tracing::{info, warn};

const TRAY_ID: &str = "koi";

/// Slow enough not to thrash the panel, fast enough to feel live.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Tray tooltips truncate on some panels; keep the reason short enough to read.
const MAX_REASON_CHARS: usize = 90;

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
fn overall_status(reports: &[MonitorReport]) -> HealthStatus {
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

/// The monitor that set the overall status — reports arrive sorted by name, so
/// the first match at that severity is stable between ticks.
fn worst_monitor(reports: &[MonitorReport], overall: HealthStatus) -> Option<&MonitorReport> {
    reports.iter().find(|r| r.status == overall)
}

/// Why that monitor is unhappy: its matching suggestion, else any suggestion,
/// else the observation key that tripped.
fn reason(report: &MonitorReport) -> Option<String> {
    let want = match report.status {
        HealthStatus::Critical => Severity::Critical,
        HealthStatus::Warning => Severity::Warning,
        HealthStatus::Healthy => return None,
    };
    let message = report
        .suggestions
        .iter()
        .find(|s| s.severity == want)
        .or_else(|| report.suggestions.first())
        .map(|s| s.message.clone())
        .or_else(|| {
            report
                .observations
                .iter()
                .find(|o| o.severity == want)
                .map(|o| o.key.clone())
        })?;
    Some(truncate(message.trim(), MAX_REASON_CHARS))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

/// One line the operator can read at a glance from the panel.
fn tooltip_for(reports: &[MonitorReport], overall: HealthStatus) -> String {
    if reports.is_empty() {
        return "koi — no monitor reports yet (run `koi check`)".to_string();
    }
    if overall == HealthStatus::Healthy {
        let n = reports.len();
        let plural = if n == 1 { "monitor" } else { "monitors" };
        return format!("koi — all {n} {plural} healthy");
    }
    let Some(worst) = worst_monitor(reports, overall) else {
        return format!("koi — {}", status_str(overall));
    };
    match reason(worst) {
        Some(why) => format!(
            "koi — {} {}: {why}",
            worst.monitor,
            status_str(worst.status)
        ),
        None => format!("koi — {} {}", worst.monitor, status_str(worst.status)),
    }
}

/// The one word the Linux panel has room for: which monitor is unhappy.
///
/// The ayatana backend behind the tray on Linux discards tooltips outright —
/// `tray-icon`'s GTK `set_tooltip` is a bare `Ok(())` and its docs say
/// "Linux: Unsupported" — so `tooltip_for`'s sentence never reaches the panel
/// (TASK-KOI203). The label beside the icon is the only text that does. Upstream
/// warns it "shouldn't be shown unless a user requests it as it can take up a
/// significant amount of space on the user's panel", so it stays empty while
/// everything is healthy and names the worst monitor when it is not. The reason
/// behind that name lives one click away in the popover.
fn panel_label_for(reports: &[MonitorReport], overall: HealthStatus) -> Option<String> {
    if overall == HealthStatus::Healthy {
        return None;
    }
    let worst = worst_monitor(reports, overall)?;
    let name = worst
        .monitor
        .strip_suffix("Monitor")
        .filter(|trimmed| !trimmed.is_empty())
        .unwrap_or(&worst.monitor);
    Some(name.to_string())
}

fn icon_for(status: HealthStatus) -> tauri::Result<Image<'static>> {
    let bytes = match status {
        HealthStatus::Healthy => ICON_GREEN,
        HealthStatus::Warning => ICON_AMBER,
        HealthStatus::Critical => ICON_RED,
    };
    Image::from_bytes(bytes)
}

/// Paint icon and tooltip from reports already in hand.
fn paint_tray(app: &AppHandle, reports: &[MonitorReport], overall: HealthStatus) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    match icon_for(overall) {
        Ok(icon) => {
            if let Err(e) = tray.set_icon(Some(icon)) {
                warn!("set_icon failed: {e}");
            }
        }
        Err(e) => warn!("icon decode failed: {e}"),
    }
    // macOS and Windows render this on hover. Linux silently drops it — see
    // panel_label_for, which carries the same information there instead.
    if let Err(e) = tray.set_tooltip(Some(tooltip_for(reports, overall))) {
        warn!("set_tooltip failed: {e}");
    }
    // Linux only: on macOS a label would duplicate the working tooltip in the
    // menu bar, and Windows ignores titles altogether.
    #[cfg(target_os = "linux")]
    if let Err(e) = tray.set_title(panel_label_for(reports, overall)) {
        warn!("set_title failed: {e}");
    }
}

/// Read the latest monitor reports and repaint the tray to match.
fn refresh_tray(app: &AppHandle) -> Result<(), String> {
    let conn = open_db()?;
    let reports = state::latest_reports_all(&conn).map_err(|e| e.to_string())?;
    let overall = overall_status(&reports);
    paint_tray(app, &reports, overall);
    Ok(())
}

/// Repaint on a slow tick so the panel reflects current state without thrash.
/// A persistent failure (no database yet) is logged once, not every tick.
fn start_refresh_tick(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last_err: Option<String> = None;
        loop {
            std::thread::sleep(REFRESH_INTERVAL);
            match refresh_tray(&app) {
                Ok(()) => last_err = None,
                Err(e) => {
                    if last_err.as_deref() != Some(e.as_str()) {
                        warn!("tray refresh: {e}");
                        last_err = Some(e);
                    }
                }
            }
        }
    });
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

    // Keep the tray in sync whenever the popover refreshes.
    paint_tray(&app, &reports, overall);

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
            let _ = refresh_tray(&app);
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
    let _ = refresh_tray(&app);
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
                // macOS and Windows only, despite the unconditional registration
                // (TASK-KOI204). The ayatana backend behind the Linux tray exposes
                // no Activate method on its StatusNotifierItem, so a panel click is
                // never delivered to the app and this arm cannot fire there.
                //
                // Linux is not left without an affordance: set_show_menu_on_left_click
                // is itself Linux-unsupported — tray-icon implements it for macOS and
                // Windows only and discards the value elsewhere — so the false above
                // does not apply and left click keeps the appindicator default of
                // opening the menu. "Open koi" then reaches toggle_popover. Two clicks
                // on Linux, one on the platforms where this handler runs. Removing the
                // false to "fix" the left click would change nothing on Linux and would
                // break the direct-open behaviour on the other two.
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

            // Paint the real health colour at startup, then keep it current.
            if let Err(e) = refresh_tray(app.handle()) {
                warn!("initial tray refresh: {e}");
            }
            start_refresh_tick(app.handle().clone());

            info!("koi system tray initialized");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running koi tray application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use koi_core::types::{Observation, Suggestion};

    fn report(monitor: &str, status: HealthStatus) -> MonitorReport {
        MonitorReport {
            monitor: monitor.into(),
            status,
            elapsed_ms: 1,
            collected_at: Utc::now(),
            observations: vec![],
            suggestions: vec![],
        }
    }

    fn with_suggestion(mut r: MonitorReport, message: &str, severity: Severity) -> MonitorReport {
        r.suggestions.push(Suggestion {
            message: message.into(),
            severity,
            action_hint: None,
        });
        r
    }

    #[test]
    fn any_critical_makes_the_whole_system_critical() {
        let reports = [
            report("CacheMonitor", HealthStatus::Healthy),
            report("DiskMonitor", HealthStatus::Critical),
            report("GitMonitor", HealthStatus::Warning),
        ];
        assert_eq!(overall_status(&reports), HealthStatus::Critical);
    }

    #[test]
    fn any_warning_without_critical_is_amber() {
        let reports = [
            report("CacheMonitor", HealthStatus::Healthy),
            report("GitMonitor", HealthStatus::Warning),
        ];
        assert_eq!(overall_status(&reports), HealthStatus::Warning);
    }

    #[test]
    fn all_healthy_is_green() {
        let reports = [
            report("CacheMonitor", HealthStatus::Healthy),
            report("DiskMonitor", HealthStatus::Healthy),
        ];
        assert_eq!(overall_status(&reports), HealthStatus::Healthy);
        assert_eq!(
            tooltip_for(&reports, HealthStatus::Healthy),
            "koi — all 2 monitors healthy"
        );
    }

    #[test]
    fn no_reports_is_green_and_says_so() {
        assert_eq!(overall_status(&[]), HealthStatus::Healthy);
        assert_eq!(
            tooltip_for(&[], HealthStatus::Healthy),
            "koi — no monitor reports yet (run `koi check`)"
        );
    }

    #[test]
    fn tooltip_names_the_worst_monitor_and_its_reason() {
        let reports = [
            with_suggestion(
                report("CacheMonitor", HealthStatus::Warning),
                "3.1 GB of stale caches",
                Severity::Warning,
            ),
            with_suggestion(
                report("DiskMonitor", HealthStatus::Critical),
                "/ is 96% full",
                Severity::Critical,
            ),
        ];
        assert_eq!(
            tooltip_for(&reports, overall_status(&reports)),
            "koi — DiskMonitor critical: / is 96% full"
        );
    }

    #[test]
    fn reason_prefers_the_suggestion_matching_the_status() {
        let mut r = with_suggestion(
            report("DiskMonitor", HealthStatus::Critical),
            "informational note",
            Severity::Info,
        );
        r = with_suggestion(r, "/ is 96% full", Severity::Critical);
        assert_eq!(reason(&r).as_deref(), Some("/ is 96% full"));
    }

    #[test]
    fn reason_falls_back_to_an_observation_key() {
        let mut r = report("MemoryMonitor", HealthStatus::Warning);
        r.observations.push(Observation {
            key: "swap_pressure".into(),
            value: serde_json::json!(0.8),
            severity: Severity::Warning,
        });
        assert_eq!(reason(&r).as_deref(), Some("swap_pressure"));
    }

    #[test]
    fn a_monitor_with_no_detail_still_gets_a_tooltip() {
        let reports = [report("DockerMonitor", HealthStatus::Warning)];
        assert_eq!(
            tooltip_for(&reports, HealthStatus::Warning),
            "koi — DockerMonitor warning"
        );
    }

    #[test]
    fn long_reasons_are_truncated_on_a_char_boundary() {
        let long = "é".repeat(MAX_REASON_CHARS + 20);
        let r = with_suggestion(
            report("FileMonitor", HealthStatus::Warning),
            &long,
            Severity::Warning,
        );
        let why = reason(&r).expect("reason");
        assert_eq!(why.chars().count(), MAX_REASON_CHARS);
        assert!(why.ends_with('…'));
    }

    #[test]
    fn every_status_icon_decodes() {
        for status in [
            HealthStatus::Healthy,
            HealthStatus::Warning,
            HealthStatus::Critical,
        ] {
            assert!(icon_for(status).is_ok(), "{status:?} icon failed to decode");
        }
    }

    #[test]
    fn refresh_interval_is_the_specified_slow_tick() {
        assert_eq!(REFRESH_INTERVAL, Duration::from_secs(30));
    }

    #[test]
    fn panel_label_stays_empty_while_healthy() {
        let reports = [
            report("CacheMonitor", HealthStatus::Healthy),
            report("DiskMonitor", HealthStatus::Healthy),
        ];
        assert_eq!(panel_label_for(&reports, HealthStatus::Healthy), None);
        assert_eq!(panel_label_for(&[], HealthStatus::Healthy), None);
    }

    #[test]
    fn panel_label_names_the_worst_monitor_when_degraded() {
        let reports = [
            report("CacheMonitor", HealthStatus::Healthy),
            report("DiskMonitor", HealthStatus::Critical),
            report("GitMonitor", HealthStatus::Warning),
        ];
        assert_eq!(
            panel_label_for(&reports, HealthStatus::Critical),
            Some("Disk".to_string())
        );
    }

    #[test]
    fn panel_label_keeps_names_that_do_not_end_in_monitor() {
        let reports = [report("Backup", HealthStatus::Warning)];
        assert_eq!(
            panel_label_for(&reports, HealthStatus::Warning),
            Some("Backup".to_string())
        );
    }
}
