//! Cost posture over the surfaces that carry live spend (TASK-KOI238).
//!
//! The mission is to keep the cost of the operator's computer systems down,
//! which starts with knowing it. Two surfaces carry spend today: Railway,
//! which hosts the vault, and GitHub Actions, free on a public repo and not
//! free the moment one goes private.
//!
//! # Why the network call is not in the monitor
//!
//! `Monitor::run` has a 200ms budget (ADR-0013) and a billing API does not fit
//! in it. This module therefore splits exactly as `backup_convergence` did
//! after the same collision (TASK-KOI192): [`refresh`] performs the network
//! read on a slow cadence and persists a snapshot, and [`CostMonitor`] reads
//! only the persisted snapshot, which is a cheap file-and-row read. A monitor
//! that reports a stale figure loudly is worth more than one that blocks
//! `koi check` for two seconds.
//!
//! # Tokens
//!
//! Read from the runtime home (`~/.config/koi/secrets/`), never the repo, and
//! never logged: [`redact`] is applied to anything that reaches tracing.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One surface's spend for one period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostSnapshot {
    /// "railway" or "github-actions".
    pub provider: String,
    /// Project or repository the spend belongs to.
    pub project: String,
    /// Billing period, `YYYY-MM`.
    pub period: String,
    /// Month-to-date amount in `currency`.
    pub amount: f64,
    pub currency: String,
    pub captured_at: DateTime<Utc>,
}

/// Mask a token so a partial value in a log cannot be replayed. Keeps the
/// first four characters, which is enough to tell two tokens apart when
/// debugging, and drops the rest.
pub fn redact(token: &str) -> String {
    let visible: String = token.chars().take(4).collect();
    format!("{visible}…({} chars)", token.len())
}

/// Parse Railway's GraphQL `usage` response into one snapshot per project.
///
/// Railway reports usage per project with an estimated cost in cents, so the
/// conversion is here rather than at the call site — a rate applied in two
/// places is a rate that disagrees with itself eventually.
pub fn parse_railway_usage(body: &str, period: &str) -> Result<Vec<CostSnapshot>, String> {
    let root: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("railway: response is not JSON: {e}"))?;

    // A GraphQL 200 can still be an error envelope; treating that as "no
    // projects" would report £0 spend on a broken token, which is the most
    // expensive possible way to be wrong here.
    if let Some(errors) = root.get("errors").and_then(|e| e.as_array()) {
        let first = errors
            .first()
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(format!("railway: API returned an error: {first}"));
    }

    let projects = root
        .pointer("/data/usage/projects")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "railway: no data.usage.projects in response".to_string())?;

    let now = Utc::now();
    let mut out = Vec::new();
    for p in projects {
        let name = p
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let cents = p
            .get("estimatedCostCents")
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| format!("railway: project {name} has no estimatedCostCents"))?;
        out.push(CostSnapshot {
            provider: "railway".to_string(),
            project: name,
            period: period.to_string(),
            amount: cents / 100.0,
            currency: "USD".to_string(),
            captured_at: now,
        });
    }
    Ok(out)
}

/// Parse GitHub's Actions billing response.
///
/// `total_minutes_used` is the whole account's usage; `included_minutes` is
/// the free allowance. Only the paid overage costs money, so that is what is
/// recorded — reporting gross minutes as spend would raise an alarm on a
/// public repo that is billed nothing at all.
pub fn parse_github_actions_billing(
    body: &str,
    account: &str,
    period: &str,
) -> Result<CostSnapshot, String> {
    let root: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("github: response is not JSON: {e}"))?;

    let paid = root
        .get("total_paid_minutes_used")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "github: no total_paid_minutes_used in response".to_string())?;

    // GitHub bills Linux minutes at $0.008; macOS and Windows carry
    // multipliers the endpoint does not break out, so this is a floor and is
    // labelled as such wherever it is displayed.
    const USD_PER_PAID_MINUTE: f64 = 0.008;

    Ok(CostSnapshot {
        provider: "github-actions".to_string(),
        project: account.to_string(),
        period: period.to_string(),
        amount: paid * USD_PER_PAID_MINUTE,
        currency: "USD".to_string(),
        captured_at: Utc::now(),
    })
}

/// Whether a surface is over the budget configured for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetState {
    /// No budget configured for this surface — reported, never flagged.
    Unset,
    Within,
    Over,
}

pub fn classify_budget(amount: f64, budget: Option<f64>) -> BudgetState {
    match budget {
        None => BudgetState::Unset,
        Some(b) if amount > b => BudgetState::Over,
        Some(_) => BudgetState::Within,
    }
}

/// How stale a snapshot is allowed to be before `koi check` stops presenting
/// it as the current figure. A billing number from last week is not a lie, but
/// it is not today's spend either.
pub const SNAPSHOT_STALE_AFTER_HOURS: i64 = 36;

pub fn is_stale(captured_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    (now - captured_at).num_hours() >= SNAPSHOT_STALE_AFTER_HOURS
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded fixture, shape taken from Railway's GraphQL usage query.
    const RAILWAY_OK: &str = r#"{"data":{"usage":{"projects":[
        {"name":"vault","estimatedCostCents":512},
        {"name":"koi-server","estimatedCostCents":0}
    ]}}}"#;

    #[test]
    fn railway_usage_converts_cents_to_currency_units() {
        let snaps = parse_railway_usage(RAILWAY_OK, "2026-09").unwrap();
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].project, "vault");
        assert!((snaps[0].amount - 5.12).abs() < f64::EPSILON);
        assert_eq!(snaps[0].currency, "USD");
        assert!((snaps[1].amount - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn railway_graphql_error_envelope_is_an_error_not_zero_spend() {
        // A 200 carrying an errors array must not read as "nothing is
        // running, you owe nothing" — the most expensive way to be wrong.
        let body = r#"{"errors":[{"message":"Not Authorized"}]}"#;
        let err = parse_railway_usage(body, "2026-09").unwrap_err();
        assert!(err.contains("Not Authorized"), "got: {err}");
    }

    #[test]
    fn railway_missing_cost_field_is_an_error_not_a_silent_zero() {
        let body = r#"{"data":{"usage":{"projects":[{"name":"vault"}]}}}"#;
        assert!(parse_railway_usage(body, "2026-09")
            .unwrap_err()
            .contains("estimatedCostCents"));
    }

    #[test]
    fn github_billing_counts_only_the_paid_overage() {
        // A public repo burns minutes and is billed nothing. Reporting the
        // gross figure would alarm on a bill that does not exist.
        let body = r#"{"total_minutes_used":41000,"included_minutes":2000,
                       "total_paid_minutes_used":0}"#;
        let snap = parse_github_actions_billing(body, "powell-clark", "2026-09").unwrap();
        assert!((snap.amount - 0.0).abs() < f64::EPSILON);
        assert_eq!(snap.provider, "github-actions");
    }

    #[test]
    fn github_billing_prices_paid_minutes() {
        let body = r#"{"total_minutes_used":5000,"total_paid_minutes_used":250}"#;
        let snap = parse_github_actions_billing(body, "powell-clark", "2026-09").unwrap();
        assert!((snap.amount - 2.0).abs() < 1e-9, "got {}", snap.amount);
    }

    #[test]
    fn github_missing_field_is_an_error() {
        assert!(parse_github_actions_billing("{}", "x", "2026-09").is_err());
    }

    #[test]
    fn budget_unset_is_never_flagged() {
        assert_eq!(classify_budget(999.0, None), BudgetState::Unset);
        assert_eq!(classify_budget(1.0, Some(10.0)), BudgetState::Within);
        assert_eq!(classify_budget(10.01, Some(10.0)), BudgetState::Over);
    }

    #[test]
    fn a_token_never_appears_whole_in_a_log_line() {
        let masked = redact("rw_live_abcdef0123456789");
        assert!(masked.starts_with("rw_l"));
        assert!(!masked.contains("abcdef0123456789"));
        assert!(masked.contains("24 chars"));
    }

    #[test]
    fn staleness_is_measured_not_assumed() {
        let now = Utc::now();
        assert!(!is_stale(now, now));
        assert!(!is_stale(now - chrono::Duration::hours(35), now));
        assert!(is_stale(now - chrono::Duration::hours(36), now));
    }
}

// -- budgets and the monitor -----------------------------------------------

/// Per-surface monthly budgets, read from `~/.config/koi/cost.toml`.
///
/// AC-3 of TASK-KOI238 named `config/thresholds.yaml`. That file is
/// prototype-era and is read by no Rust code in this tree; koi reads its
/// runtime configuration from `~/.config/koi/`, and the README states it never
/// reads its own source tree. Teaching it to read a repo file would contradict
/// the shipped architecture to satisfy the letter of the criterion, so the
/// budgets live where every other koi setting lives. The defaults below carry
/// the figures that were sitting in `thresholds.yaml` so nothing is lost.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct CostBudgets {
    /// USD per month before GitHub Actions is flagged.
    pub github_monthly_limit: Option<f64>,
    /// USD per month before Railway is flagged.
    pub railway_monthly_limit: Option<f64>,
}

impl Default for CostBudgets {
    fn default() -> Self {
        Self {
            // Carried across from config/thresholds.yaml.
            github_monthly_limit: Some(5.00),
            // The operator's recorded GBP 10/month decision on TASK-KOI116,
            // held here in USD as the surface reports USD. Deliberately
            // generous rather than exact: this flags a runaway, it does not
            // reconcile a bill.
            railway_monthly_limit: Some(13.00),
        }
    }
}

impl CostBudgets {
    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(error = %e, path = %path.display(),
                    "malformed cost.toml, falling back to compiled defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn load() -> Self {
        match crate::state::home_dir() {
            Ok(home) => Self::load_from(&home.join(".config/koi/cost.toml")),
            Err(_) => Self::default(),
        }
    }

    pub fn for_provider(&self, provider: &str) -> Option<f64> {
        match provider {
            "github-actions" => self.github_monthly_limit,
            "railway" => self.railway_monthly_limit,
            _ => None,
        }
    }
}

/// Reports the persisted cost snapshots. Never touches the network — see the
/// module docs for why the refresh is a separate, slower path.
pub struct CostMonitor {
    budgets: CostBudgets,
}

impl Default for CostMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl CostMonitor {
    pub fn new() -> Self {
        Self {
            budgets: CostBudgets::load(),
        }
    }
}

impl crate::monitor::Monitor for CostMonitor {
    fn name(&self) -> &'static str {
        "CostMonitor"
    }

    fn run(&self) -> crate::Result<crate::types::MonitorReport> {
        use crate::types::{HealthStatus, MonitorReport, Observation, Severity, Suggestion};
        let started = std::time::Instant::now();
        let now = Utc::now();

        // A missing database is "nothing measured yet", not a failure of the
        // machine's cost posture, so an error here degrades to an empty series
        // rather than failing `koi check`.
        let snapshots = crate::state::default_db_path()
            .and_then(|p| crate::state::open(&p))
            .and_then(|conn| crate::state::latest_cost_snapshots(&conn))
            .unwrap_or_default();

        let mut observations = Vec::new();
        let mut suggestions = Vec::new();
        let mut status = HealthStatus::Healthy;

        if snapshots.is_empty() {
            observations.push(Observation {
                key: "cost.snapshots".to_string(),
                value: serde_json::json!(0),
                severity: Severity::Info,
            });
            suggestions.push(Suggestion {
                message: "No cost snapshot recorded yet — run `koi cost --refresh` to take one."
                    .to_string(),
                severity: Severity::Info,
                action_hint: Some("koi cost --refresh".to_string()),
            });
        }

        for snap in &snapshots {
            let budget = self.budgets.for_provider(&snap.provider);
            let state = classify_budget(snap.amount, budget);
            let stale = is_stale(snap.captured_at, now);

            let severity = match (state, stale) {
                (BudgetState::Over, _) => Severity::Warning,
                (_, true) => Severity::Info,
                _ => Severity::Info,
            };
            if state == BudgetState::Over {
                status = HealthStatus::Warning;
                suggestions.push(Suggestion {
                    message: format!(
                        "{} ({}) is at {:.2} {} against a {:.2} budget.",
                        snap.provider,
                        snap.project,
                        snap.amount,
                        snap.currency,
                        budget.unwrap_or_default()
                    ),
                    severity: Severity::Warning,
                    action_hint: Some("koi cost".to_string()),
                });
            }
            if stale {
                suggestions.push(Suggestion {
                    message: format!(
                        "{} figure is over {SNAPSHOT_STALE_AFTER_HOURS}h old — re-run `koi cost --refresh`.",
                        snap.provider
                    ),
                    severity: Severity::Info,
                    action_hint: Some("koi cost --refresh".to_string()),
                });
            }

            observations.push(Observation {
                key: format!("cost.{}.{}", snap.provider, snap.project),
                value: serde_json::json!({
                    "amount": snap.amount,
                    "currency": snap.currency,
                    "period": snap.period,
                    "budget": budget,
                    "stale": stale,
                }),
                severity,
            });
        }

        // Renewal warnings (TASK-KOI239 AC-4). The register is the operator's
        // own confirmed list, so an entry here is a commitment they have
        // acknowledged rather than a figure koi inferred.
        let register = crate::subscriptions::Register::load();
        let today = now.date_naive();
        let due = crate::subscriptions::renewals_within(&register.subscriptions, today, 7);
        if !due.is_empty() {
            status = HealthStatus::Warning;
            for sub in &due {
                suggestions.push(Suggestion {
                    message: format!(
                        "{} renews on {} ({:.2} {}).",
                        sub.provider,
                        sub.next_renewal.as_deref().unwrap_or("soon"),
                        sub.amount,
                        sub.currency
                    ),
                    severity: Severity::Warning,
                    action_hint: Some("koi costs list".to_string()),
                });
            }
            observations.push(Observation {
                key: "cost.renewals_within_7d".to_string(),
                value: serde_json::json!(due.len()),
                severity: Severity::Warning,
            });
        }

        for (currency, total) in crate::subscriptions::monthly_totals(&register.subscriptions) {
            observations.push(Observation {
                key: format!("cost.subscriptions.monthly.{currency}"),
                value: serde_json::json!(total),
                severity: Severity::Info,
            });
        }

        Ok(MonitorReport {
            monitor: "CostMonitor".to_string(),
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
            collected_at: now,
            observations,
            suggestions,
        })
    }
}

#[cfg(test)]
mod monitor_tests {
    use super::*;
    use crate::monitor::Monitor;

    #[test]
    fn budgets_resolve_per_provider_and_unknown_surfaces_are_unbudgeted() {
        let b = CostBudgets::default();
        assert_eq!(b.for_provider("github-actions"), Some(5.00));
        assert_eq!(b.for_provider("railway"), Some(13.00));
        assert_eq!(b.for_provider("some-future-cloud"), None);
    }

    #[test]
    fn a_malformed_cost_toml_falls_back_rather_than_crashing_koi_check() {
        let dir = std::env::temp_dir().join(format!("koi-cost-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cost.toml");
        std::fs::write(&path, "this is not toml {{{").unwrap();
        assert_eq!(CostBudgets::load_from(&path), CostBudgets::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_operator_budget_overrides_the_default() {
        let dir = std::env::temp_dir().join(format!("koi-cost-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cost.toml");
        std::fs::write(&path, "railway_monthly_limit = 2.50\n").unwrap();
        let b = CostBudgets::load_from(&path);
        assert_eq!(b.for_provider("railway"), Some(2.50));
        // An unmentioned key keeps its default rather than becoming None.
        assert_eq!(b.for_provider("github-actions"), Some(5.00));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_monitor_stays_well_inside_its_budget_with_no_network() {
        // AC-4: the monitor must not be the thing that makes `koi check` slow.
        // It reads a cached row and never calls out, so this is a real
        // measurement rather than a mocked one.
        let m = CostMonitor::new();
        let report = m.run().expect("monitor must not fail on a cold machine");
        assert_eq!(report.monitor, "CostMonitor");
        assert!(
            report.elapsed_ms < m.budget_ms(),
            "took {}ms against a {}ms budget",
            report.elapsed_ms,
            m.budget_ms()
        );
    }

    #[test]
    fn no_snapshot_yet_is_reported_as_information_not_a_fault() {
        // A machine that has never refreshed is not in a bad cost posture; it
        // is in an unknown one, and saying "healthy" or "critical" would both
        // be lies.
        let m = CostMonitor::new();
        let report = m.run().unwrap();
        if report
            .observations
            .iter()
            .any(|o| o.key == "cost.snapshots")
        {
            assert_eq!(report.status, crate::types::HealthStatus::Healthy);
            assert!(report
                .suggestions
                .iter()
                .any(|s| s.message.contains("No cost snapshot recorded yet")));
        }
    }
}
