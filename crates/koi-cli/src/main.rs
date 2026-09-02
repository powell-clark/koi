use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use koi_core::{
    cleaners,
    config::FilingConfig,
    dedupe,
    filing::{
        self, DocumentsMonitor, DownloadsMonitor, FileMonitor, GoogleDriveMonitor, InboxMonitor,
        Outcome, ProposalId, ProposedAction, RootClutterMonitor, ScanContext, SqliteClassifier,
    },
    monitors::{
        BackupMonitor, CacheMonitor, DiskMonitor, DockerMonitor, GhosttyMonitor, GitMonitor,
        LatencyMonitor, MemoryMonitor, ModelSizeMonitor, NetworkMonitor, PackageMonitor,
        UnitMonitor, WezTermMonitor,
    },
    notes, state, trash,
    types::HealthStatus,
    Monitor,
};

#[derive(Parser)]
#[command(
    name = "koi",
    version,
    about = "System health, maintenance, and file lifecycle"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum CostsAction {
    /// List the register with monthly-equivalent totals per currency.
    List,
    /// Read receipts already filed on disk and propose register rows.
    /// Writes candidates as unconfirmed; nothing enters a total until
    /// `koi costs confirm` accepts it.
    Seed {
        /// Directories to read receipts from (default: Documents/PDFs-Inbox,
        /// Documents/PDFs, inbox).
        #[arg(long)]
        dir: Vec<std::path::PathBuf>,
    },
    /// Confirm a seeded row by provider name, so it counts toward totals.
    Confirm { provider: String },
}

#[derive(Subcommand)]
enum Command {
    /// Run system health diagnostics.
    Check {
        /// Emit machine-readable JSON instead of human summary.
        #[arg(long)]
        json: bool,
    },
    /// Generate detailed markdown health report.
    Report {
        /// Write to file (or directory — auto-names with timestamp). Default: stdout.
        #[arg(long)]
        output: Option<std::path::PathBuf>,
    },
    /// Execute cleanup routines.
    Clean {
        #[arg(long)]
        dry_run: bool,
    },
    /// Back up amber-tier data to encrypted Google Drive.
    Backup {
        /// List files to upload without executing sync.
        #[arg(long)]
        dry_run: bool,
        /// Include red-tier sensitive data (secrets, keys). Requires explicit confirmation.
        #[arg(long)]
        include_red: bool,
        /// Measure how far the encrypted remote has converged on the local
        /// source and persist the result for `koi check`. Syncs nothing.
        #[arg(long)]
        status: bool,
    },
    /// Start continuous health monitoring.
    Monitor,
    /// Scan accumulation points and emit filing proposals (no mutations).
    Scan {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Print the filing taxonomy - what "organised" means on this machine -
        /// and exit without scanning.
        #[arg(long)]
        explain: bool,
    },
    /// List pending filing proposals.
    Proposals {
        /// Only show proposals from this monitor.
        #[arg(long)]
        monitor: Option<String>,
        /// Cap the number of proposals shown.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Approve and execute one or all pending proposals.
    Approve {
        /// Apply every pending proposal.
        #[arg(long, conflicts_with = "id")]
        all: bool,
        /// Show what would happen without touching files.
        #[arg(long)]
        dry_run: bool,
        /// Cap the number of proposals applied in one invocation.
        #[arg(long)]
        limit: Option<usize>,
        /// With --all, only approve proposals from this monitor.
        #[arg(long, requires = "all")]
        monitor: Option<String>,
        /// With --all, also sweep content-bearing proposals (Downloads,
        /// Documents, inbox, Drive). Off by default: filing is extension-based,
        /// so a bank statement and a screenshot look identical to the batch path.
        #[arg(long, requires = "all")]
        include_sensitive: bool,
        /// Proposal id (hex prefix).
        id: Option<String>,
    },
    /// Reject a pending proposal (records signal, does nothing on disk).
    Reject {
        /// Proposal id (hex prefix). Required unless --all is set.
        id: Option<String>,
        /// Reject every pending proposal.
        #[arg(long, conflicts_with = "id")]
        all: bool,
        /// With --all, only reject proposals from this monitor.
        #[arg(long, requires = "all")]
        monitor: Option<String>,
    },
    /// Show recent persisted reports for a monitor.
    History {
        monitor: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Show aggregate stats — proposals by state, decisions by kind, learning data.
    Stats,
    /// Print shell completion script.
    ///
    /// Example: `koi completions bash > ~/.local/share/bash-completion/completions/koi`
    Completions { shell: Shell },
    /// Git-object hygiene across ~/projects — counts loose objects and, with
    /// --apply, runs a conservative gc (TASK-KOI234).
    GitGc {
        /// Actually run the gc. Without this, nothing is changed.
        #[arg(long)]
        apply: bool,
        /// Loose-object count above which a repo is collected.
        #[arg(long, default_value_t = koi_core::cleaners::git_objects::DEFAULT_LOOSE_THRESHOLD)]
        threshold: u64,
    },
    /// Show this machine's place in the declared fleet (TASK-KOI158).
    Fleet,
    /// Subscription and renewal register (TASK-KOI239).
    Costs {
        #[command(subcommand)]
        action: CostsAction,
    },
    /// Show cost posture per surface; --refresh reads the billing APIs first.
    Cost {
        /// Call the Railway and GitHub billing APIs and persist a fresh
        /// snapshot. Without this flag the command only reads what is stored,
        /// so it is safe to run in a loop.
        #[arg(long)]
        refresh: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// List detected managed zones — directories claimed by other systems via
    /// `.koi-managed-by` markers, which koi will not touch.
    Zones {
        /// Roots to search (defaults to ~/Documents, ~/Downloads, ~/inbox, ~/Desktop).
        #[arg(long)]
        root: Vec<std::path::PathBuf>,
    },
    /// Show recent worklog entries (worklog.jsonl in the koi data dir).
    Worklog {
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Show recent incident entries (incidents.jsonl in the koi data dir).
    Incidents {
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Run a Lynis security audit and persist the hardening score.
    Audit {
        /// Run a faster scan covering only authentication, filesystems, and networking.
        #[arg(long)]
        quick: bool,
    },
    /// Manage koi systemd user timers.
    Timers {
        #[command(subcommand)]
        action: TimerAction,
    },
    /// Print paths koi reads and writes (useful for debugging + shell scripts).
    Paths,
    /// Manage personal notes in ~/notes/ (plain Markdown, owned format).
    Notes {
        #[command(subcommand)]
        action: NotesAction,
    },
    /// Find and persist duplicate-file groups (read-only; see ADR-0021).
    Dedupe {
        #[command(subcommand)]
        action: DedupeAction,
    },
    /// Manage koi's reversible trash (see ADR-0021).
    Trash {
        #[command(subcommand)]
        action: TrashAction,
    },
}

#[derive(Subcommand)]
enum DedupeAction {
    /// Scan for cross-root duplicate-content groups (never mutates files).
    Scan {
        /// Roots to search (defaults to configured/`$HOME` Downloads, Documents, inbox).
        #[arg(long)]
        root: Vec<std::path::PathBuf>,
    },
    /// Move every non-keeper member of one or more persisted groups to trash.
    Apply {
        /// Group id (hex prefix, matching a `koi dedupe scan` result).
        group: Option<String>,
        /// Apply to every persisted group.
        #[arg(long, conflicts_with = "group")]
        all_groups: bool,
        /// Show what would move without touching anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum TrashAction {
    /// List entries currently in trash (never restored).
    List,
    /// Restore a trash entry to its original path.
    Restore { id: i64 },
    /// Permanently delete trash entries older than a window — the only
    /// delete-shaped operation in koi. Requires --yes; without it, previews
    /// what would be removed and deletes nothing.
    Empty {
        /// e.g. "30d", "12h" — entries trashed longer ago than this are
        /// removed. Defaults to `[trash] retention_days` from filing.toml
        /// (30d) when omitted.
        #[arg(long)]
        older_than: Option<String>,
        /// Actually delete. Without this flag, Empty only previews.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum NotesAction {
    /// Create a new note.
    New {
        /// Note title.
        title: String,
        /// Open the note in $EDITOR after creating.
        #[arg(long)]
        edit: bool,
    },
    /// List recent notes.
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Search notes by title or body text.
    Search { query: String },
}

#[derive(Subcommand)]
enum TimerAction {
    /// Install and enable the koi audit timers (weekly quick + monthly full).
    InstallAudit {
        /// Show what would be installed without making changes.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show status of all installed koi timers.
    Status,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Check { json } => run_check(json)?,
        Command::Report { output } => run_report(output)?,
        Command::Clean { dry_run } => run_clean(dry_run)?,
        Command::Backup {
            dry_run,
            include_red,
            status,
        } => run_backup(dry_run, include_red, status)?,
        Command::Monitor => run_monitor()?,
        Command::Scan { json, explain } => run_scan(json, explain)?,
        Command::Proposals { monitor, limit } => run_proposals(monitor, limit)?,
        Command::Approve {
            all,
            dry_run,
            limit,
            monitor,
            include_sensitive,
            id,
        } => run_approve(all, dry_run, limit, monitor, include_sensitive, id)?,
        Command::Reject { id, all, monitor } => run_reject(id, all, monitor)?,
        Command::History { monitor, limit } => run_history(&monitor, limit)?,
        Command::Stats => run_stats()?,
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "koi", &mut std::io::stdout());
        }
        Command::GitGc { apply, threshold } => run_git_gc(apply, threshold)?,
        Command::Fleet => run_fleet()?,
        Command::Costs { action } => run_costs(action)?,
        Command::Cost { refresh, json } => run_cost(refresh, json)?,
        Command::Zones { root } => run_zones(root)?,
        Command::Worklog { limit } => run_jsonl_tail(
            &state::default_data_dir()?.join("worklog.jsonl"),
            limit,
            "Worklog",
        )?,
        Command::Incidents { limit } => run_jsonl_tail(
            &state::default_data_dir()?.join("incidents.jsonl"),
            limit,
            "Incidents",
        )?,
        Command::Audit { quick } => run_audit(quick)?,
        Command::Timers { action } => run_timers(action)?,
        Command::Paths => run_paths()?,
        Command::Notes { action } => run_notes(action)?,
        Command::Dedupe { action } => run_dedupe(action)?,
        Command::Trash { action } => run_trash(action)?,
    }
    Ok(())
}

fn run_monitor() -> Result<()> {
    // `koi monitor` = "show me the daemon's current state". We inspect running
    // processes for koi-daemon and show the latest log file if available.
    use koi_core::monitors::process_family_stats;

    let stats = process_family_stats("koi-daemon");
    if stats.count == 0 {
        println!("koi-daemon not running.");
        println!();
        println!("Start with:");
        println!("  systemctl --user start koi-daemon.service    # Linux");
        println!(
            "  launchctl load ~/Library/LaunchAgents/com.powellclark.koi.daemon.plist  # macOS"
        );
        println!();
        println!("Or run interactively:");
        println!("  ./target/release/koi-daemon");
        return Ok(());
    }

    println!("koi-daemon running:");
    if let Some(top) = &stats.top {
        println!(
            "  pid:  {}\n  rss:  {:.1} MiB\n  name: {}",
            top.pid,
            (top.rss as f64) / (1024f64.powi(2)),
            top.name
        );
    }

    let log_dir = directories::ProjectDirs::from("com", "powellclark", "koi")
        .map(|d| d.data_dir().join("logs"));
    if let Some(dir) = log_dir {
        if dir.exists() {
            println!("\nLatest log entries ({}):", dir.display());
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut files: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .starts_with("koi-daemon.log.")
                    })
                    .collect();
                files.sort_by_key(|e| e.file_name());
                if let Some(latest) = files.last() {
                    if let Ok(text) = std::fs::read_to_string(latest.path()) {
                        for line in text.lines().rev().take(5).collect::<Vec<_>>().iter().rev() {
                            println!("  {line}");
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn run_audit(quick: bool) -> Result<()> {
    use koi_core::state::{record_audit_run, NewAuditRun};

    // Warn (but do not abort) when not root — Lynis runs with reduced scope.
    let euid = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(1);
    if euid != 0 {
        eprintln!(
            "warn: running as non-root — some Lynis checks will be skipped. For full coverage: sudo koi audit{}",
            if quick { " --quick" } else { "" }
        );
    }

    // Locate or create the audits directory.
    let home = koi_core::state::home_dir()?;
    let audits_dir = home.join(".local/share/koi/audits");
    std::fs::create_dir_all(&audits_dir).context("create audits directory")?;

    let ts = chrono::Utc::now();
    let ts_str = ts.format("%Y%m%d-%H%M%S").to_string();
    let report_path = audits_dir.join(format!("lynis-{ts_str}.log"));

    println!("Running Lynis{}…", if quick { " (quick)" } else { "" });

    // Machine-readable report written alongside the human-readable log. Lynis
    // defaults this to /var/log/lynis-report.dat, which is root-only and so
    // unreadable on the non-root path this command actively supports.
    let report_dat_path = audits_dir.join(format!("lynis-{ts_str}.dat"));
    let report_log_path = audits_dir.join(format!("lynis-{ts_str}.lynis.log"));

    let mut cmd = std::process::Command::new("lynis");
    cmd.arg("audit")
        .arg("system")
        .arg("--no-colors")
        .arg("--report-file")
        .arg(&report_dat_path)
        // --logfile matters as much as --report-file. Lynis defaults its log
        // to /var/log/lynis.log and, running unprivileged, falls back to the
        // working directory — which for koi-audit-quick.service is %h. That is
        // why ~/lynis.log kept reappearing at HOME root and why
        // RootClutterMonitor kept proposing to move it (TASK-KOI225): the
        // proposal was treating a symptom whose writer was still live.
        .arg("--logfile")
        .arg(&report_log_path);
    if quick {
        cmd.args([
            "--tests-from-group",
            "authentication,filesystems,networking",
        ]);
    }

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!(
                "lynis is not installed or not on PATH — install it with: sudo apt install lynis"
            )
        } else {
            anyhow::Error::new(e).context("spawn lynis")
        }
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, String::from_utf8_lossy(&output.stderr));

    // Write raw log to file.
    std::fs::write(&report_path, combined.as_bytes()).context("write audit log")?;

    // Read the score from the machine-readable report first, falling back to
    // scraping stdout. `--quiet` used to be passed here, which suppressed the
    // stdout summary this parser needs, so every run from 2026-08-16 onward
    // recorded ?/100 while appearing to succeed (TASK-KOI108).
    let report_dat = std::fs::read_to_string(&report_dat_path).ok();
    let hardening_index = report_dat
        .as_deref()
        .and_then(parse_report_hardening_index)
        .or_else(|| parse_lynis_hardening_index(&stdout));
    let lynis_version = parse_lynis_version(&stdout);

    match hardening_index {
        Some(score) => println!("Hardening index: {score}/100"),
        // TASK-KOI233 AC-4. "(not found in output)" used to cover three
        // different situations, so a year of logs could not tell a broken
        // parser from a scan that never got far enough to score itself. Each
        // now names what actually happened and what to do about it.
        None => println!(
            "Hardening index: {}",
            describe_missing_index(report_dat.as_deref())
        ),
    }
    println!("Report saved: {}", report_path.display());

    // Persist to SQLite (best-effort) and emit notification if score dropped.
    let prev_score = match open_state() {
        Ok(conn) => {
            // Read previous score before writing the new one.
            let prev = koi_core::state::recent_audit_runs(&conn, 1)
                .ok()
                .and_then(|runs| runs.into_iter().next())
                .and_then(|r| r.hardening_index);

            let run = NewAuditRun {
                ran_at: ts,
                hardening_index,
                quick,
                report_path: report_path.to_string_lossy().into_owned(),
                lynis_version: lynis_version.unwrap_or_default(),
            };
            if let Err(e) = record_audit_run(&conn, &run) {
                eprintln!("warn: failed to persist audit run: {e}");
            }
            prev
        }
        Err(e) => {
            eprintln!("warn: state unavailable, audit not persisted: {e}");
            None
        }
    };

    // Desktop notification when the score drops or Lynis raised real warnings.
    // Counted from the structured report, falling back to the stdout scrape only
    // if the report is unreadable — that scrape is a substring match and has
    // produced a false critical alert (TASK-KOI108).
    let warning_count = std::fs::read_to_string(&report_dat_path)
        .ok()
        .map(|dat| parse_report_warning_count(&dat))
        .unwrap_or_else(|| count_lynis_warnings(&stdout, "warning"));
    audit_notify(hardening_index, prev_score, warning_count, &report_path);

    Ok(())
}

/// Count Lynis output lines matching a severity keyword.
fn count_lynis_warnings(output: &str, keyword: &str) -> usize {
    output
        .lines()
        .filter(|l| l.to_ascii_lowercase().contains(keyword))
        .count()
}

/// Emit a desktop notification if the audit result warrants attention.
///
/// - Score drop of any amount → notify with old→new.
/// - Critical warnings found regardless of score → notify.
/// - Score unchanged or improved with no criticals → silent (avoids alert fatigue).
fn audit_notify(
    current: Option<i64>,
    previous: Option<i64>,
    critical_count: usize,
    report_path: &std::path::Path,
) {
    let should_notify = match (current, previous) {
        (Some(cur), Some(prev)) => cur < prev || critical_count > 0,
        (Some(_), None) => critical_count > 0, // first run: only notify on criticals
        _ => false,
    };

    if !should_notify {
        return;
    }

    let score_msg = match (current, previous) {
        (Some(cur), Some(prev)) if cur < prev => format!("Score: {prev} → {cur}/100"),
        (Some(cur), None) => format!("Score: {cur}/100 (first run)"),
        (Some(cur), _) => format!("Score: {cur}/100"),
        _ => "Score: unknown".into(),
    };

    let body = if critical_count > 0 {
        format!(
            "{score_msg} | {critical_count} warning(s)
Report: {}",
            report_path.display()
        )
    } else {
        format!(
            "{score_msg}
Report: {}",
            report_path.display()
        )
    };

    let urgency =
        if critical_count > 0 || matches!((current, previous), (Some(c), Some(p)) if c < p - 5) {
            "critical"
        } else {
            "normal"
        };

    let _ = std::process::Command::new("notify-send")
        .args([
            "--app-name=koi",
            &format!("--urgency={urgency}"),
            "Koi Security Audit",
            &body,
        ])
        .status();
}

/// Say WHY there is no hardening index, distinguishing the three cases that
/// used to share one message (TASK-KOI233 AC-4):
///
/// - no report file at all: Lynis did not get far enough to write one, so the
///   scan failed rather than the parser
/// - a report that carries no `hardening_index` key: Lynis ran but did not
///   score the host, which is what a partial or aborted profile looks like
/// - a report carrying the key with a value that will not parse: a genuine
///   parse failure, and the only one of the three that is koi's bug
fn describe_missing_index(report_dat: Option<&str>) -> &'static str {
    let Some(dat) = report_dat else {
        return "(no report file written — the Lynis run itself failed)";
    };
    match dat
        .lines()
        .filter_map(|line| line.split_once('='))
        .find(|(key, _)| key.trim() == "hardening_index")
    {
        None => "(not scored by Lynis — the report has no hardening_index)",
        Some(_) => "(unparseable hardening_index in the report — this is a koi bug)",
    }
}

// Read the hardening index out of a Lynis `--report-file` .dat, which is
// key=value and machine-readable, rather than out of human-facing stdout.
//
// This is the primary source because the stdout summary is suppressed by
// `--quiet`, and parsing a report meant for humans was how the score came to be
// silently missing on every run since 2026-08-16 (TASK-KOI108). The key must
// match exactly: Lynis also writes `hardening_index_previous`, and a prefix
// match would happily return the PREVIOUS run's score as if it were this one.
fn parse_report_hardening_index(report: &str) -> Option<i64> {
    report
        .lines()
        .filter_map(|line| line.split_once('='))
        .find(|(key, _)| key.trim() == "hardening_index")
        .and_then(|(_, value)| value.trim().parse::<i64>().ok())
}

// Count real Lynis warnings from the machine-readable report.
//
// Lynis distinguishes `warning[]=` (findings needing attention) from
// `suggestion[]=` (advisory). Counting the structured key is exact, where
// substring-searching human output is not: on 2026-08-20 the old counter
// scanned stdout for the word "critical" and matched the SUGGESTION "Install
// apt-listbugs to display a list of critical bugs", which raised a
// critical-urgency desktop notification on a scan that had zero warnings.
// Alert fatigue starts with alerts that were never real.
fn parse_report_warning_count(report: &str) -> usize {
    report
        .lines()
        .filter(|line| line.starts_with("warning[]="))
        .count()
}

/// Extract the Lynis hardening index from stdout — the fallback source.
/// Looks for a line containing "Hardening index" and takes its first number.
fn parse_lynis_hardening_index(output: &str) -> Option<i64> {
    for line in output.lines() {
        if line.to_ascii_lowercase().contains("hardening index") {
            // Extract the first number in the line.
            for part in line.split_whitespace() {
                let digits: String = part.chars().filter(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = digits.parse::<i64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// Extract the Lynis version from stdout.
fn parse_lynis_version(output: &str) -> Option<String> {
    for line in output.lines() {
        if line.to_ascii_lowercase().contains("lynis") && line.contains("version") {
            // "Lynis 3.0.9" or "Version: 3.0.9"
            let parts: Vec<&str> = line.split_whitespace().collect();
            for (i, p) in parts.iter().enumerate() {
                if p.eq_ignore_ascii_case("version") || p.eq_ignore_ascii_case("lynis") {
                    if let Some(v) = parts.get(i + 1) {
                        let cleaned =
                            v.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
                        if !cleaned.is_empty() {
                            return Some(cleaned.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

fn run_timers(action: TimerAction) -> Result<()> {
    match action {
        TimerAction::InstallAudit { dry_run } => install_audit_timers(dry_run),
        TimerAction::Status => show_timer_status(),
    }
}

fn install_audit_timers(dry_run: bool) -> Result<()> {
    use std::path::PathBuf;

    let home: PathBuf = koi_core::state::home_dir()?;

    // Source: share/systemd/ relative to the project root (where this binary lives).
    let bin = std::env::current_exe().context("resolve binary path")?;
    let project_root = bin
        .ancestors()
        .find(|p| p.join("share/systemd").is_dir())
        .map(|p| p.to_path_buf())
        .ok_or_else(|| anyhow::anyhow!(
            "cannot find share/systemd/ from binary path {}. Is koi running from the project directory?",
            bin.display()
        ))?;

    let units = [
        "koi-audit-quick.service",
        "koi-audit-quick.timer",
        "koi-audit-full.service",
        "koi-audit-full.timer",
    ];
    let dest_dir = home.join(".config/systemd/user");

    if !dry_run {
        std::fs::create_dir_all(&dest_dir).context("create ~/.config/systemd/user")?;
    }

    for unit in &units {
        let src = project_root.join("share/systemd").join(unit);
        let dst = dest_dir.join(unit);
        if dry_run {
            println!("would install: {} -> {}", src.display(), dst.display());
        } else {
            std::fs::copy(&src, &dst)
                .with_context(|| format!("copy {unit} to {}", dst.display()))?;
            println!("installed: {}", dst.display());
        }
    }

    if !dry_run {
        // Reload systemd so the new units are visible.
        let reload = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status()
            .context("systemctl daemon-reload")?;
        if !reload.success() {
            eprintln!("warn: daemon-reload exited non-zero — units may not be active yet");
        }

        // Enable and start both timers.
        let enable = std::process::Command::new("systemctl")
            .args([
                "--user",
                "enable",
                "--now",
                "koi-audit-quick.timer",
                "koi-audit-full.timer",
            ])
            .status()
            .context("systemctl enable --now")?;
        if enable.success() {
            println!(
                "Timers enabled. Run `systemctl --user list-timers | grep koi-audit` to verify."
            );
        } else {
            eprintln!("warn: systemctl enable exited non-zero");
        }
    } else {
        println!("Dry run complete. Remove --dry-run to install.");
    }

    Ok(())
}

fn show_timer_status() -> Result<()> {
    let out = std::process::Command::new("systemctl")
        .args(["--user", "list-timers", "--no-legend"])
        .output()
        .context("systemctl list-timers")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let koi_timers: Vec<&str> = text.lines().filter(|l| l.contains("koi")).collect();
    if koi_timers.is_empty() {
        println!("No koi timers installed. Run: koi timers install-audit");
    } else {
        println!("Active koi timers:");
        for t in koi_timers {
            println!("  {t}");
        }
    }
    Ok(())
}

fn run_notes(action: NotesAction) -> Result<()> {
    let root = notes::default_notes_dir()
        .ok_or_else(|| anyhow::anyhow!("$HOME not set — cannot locate notes directory"))?;
    match action {
        NotesAction::New { title, edit } => {
            let path = notes::create_note(&root, &title)?;
            println!("{}", path.display());
            if edit {
                let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".into());
                std::process::Command::new(&editor)
                    .arg(&path)
                    .status()
                    .with_context(|| format!("open {editor}"))?;
            }
        }
        NotesAction::List { limit } => {
            let all = notes::list_notes(&root)?;
            if all.is_empty() {
                println!("No notes yet. Create one with: koi notes new \"My note title\"");
                return Ok(());
            }
            println!("{} note(s) in {}:", all.len().min(limit), root.display());
            for n in all.iter().take(limit) {
                println!("  {}  {}", n.modified.format("%Y-%m-%d"), n.title);
            }
        }
        NotesAction::Search { query } => {
            let results = notes::search_notes(&root, &query)?;
            if results.is_empty() {
                println!("No notes matching \"{query}\".");
                return Ok(());
            }
            println!("{} match(es) for \"{}\":", results.len(), query);
            for n in &results {
                println!("  {}  {}", n.modified.format("%Y-%m-%d"), n.title);
                println!("       {}", n.path.display());
            }
        }
    }
    Ok(())
}

fn run_paths() -> Result<()> {
    let home = koi_core::state::home_dir()?;
    let db = state::default_db_path().ok();
    let data = directories::ProjectDirs::from("com", "powellclark", "koi")
        .map(|d| d.data_dir().to_path_buf());
    let cache = directories::ProjectDirs::from("com", "powellclark", "koi")
        .map(|d| d.cache_dir().to_path_buf());

    let pairs: [(&str, Option<std::path::PathBuf>); 7] = [
        ("home", Some(home.clone())),
        ("db", db),
        ("data_dir", data.clone()),
        ("cache_dir", cache),
        ("logs_dir", data.as_ref().map(|d| d.join("logs"))),
        (
            "disk_cache_file",
            Some(home.join(".cache/koi/disk-cache.json")),
        ),
        (
            "package_cache",
            Some(home.join(".cache/koi/package-monitor.json")),
        ),
    ];
    for (label, path) in &pairs {
        match path {
            Some(p) => println!("{:<16} {}", label, p.display()),
            None => println!("{:<16} —", label),
        }
    }
    Ok(())
}

fn run_jsonl_tail(path: &std::path::Path, limit: usize, label: &str) -> Result<()> {
    // Operational state lives under the XDG data dir (ADR-0019), never in the
    // source tree — so the path is resolved by the caller and read directly,
    // with no dependency on the current working directory.
    if !path.exists() {
        println!("# {label} (last 0):");
        println!();
        println!("_No entries yet at {}._", path.display());
        return Ok(());
    }

    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(limit);
    let recent = &lines[start..];

    println!("# {label} (last {}):", recent.len());
    println!();
    for line in recent {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            println!("{line}");
            continue;
        };
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("?");
        let ts = v.get("timestamp").and_then(|x| x.as_str()).unwrap_or("?");
        let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
        println!("- **{id}** ({ts}) — {title}");
        if let Some(changes) = v.get("changes").and_then(|x| x.as_array()) {
            for c in changes.iter().take(5) {
                if let Some(s) = c.as_str() {
                    println!("    - {s}");
                }
            }
            if changes.len() > 5 {
                println!("    - ... {} more", changes.len() - 5);
            }
        }
    }
    Ok(())
}

/// Read a secret from the runtime home. Never the repo, never an argument —
/// an argument would put the token in the process table and the shell history.
fn read_cost_secret(name: &str) -> Option<String> {
    let home = koi_core::state::home_dir().ok()?;
    let path = home.join(".config/koi/secrets").join(name);
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// koi already shells out to rclone, railway, systemctl and lynis, so curl is
/// the idiom here too and costs no new dependency on a public repo.
fn http_post_json(url: &str, token: &str, body: &str) -> Result<String> {
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "20",
            "-X",
            "POST",
            url,
            "-H",
            "Content-Type: application/json",
            // The token goes in an argument to curl, not to a shell, so it is
            // not expanded or logged by koi. It is visible in this process's
            // own argv for the lifetime of the call, which is the same
            // exposure the railway CLI already has.
            "-H",
            &format!("Authorization: Bearer {token}"),
            "--data-binary",
            body,
        ])
        .output()
        .context("curl not available — install it or set the surface aside")?;
    if !out.status.success() {
        anyhow::bail!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn http_get_json(url: &str, token: &str) -> Result<String> {
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "20",
            url,
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            &format!("Authorization: Bearer {token}"),
        ])
        .output()
        .context("curl not available — install it or set the surface aside")?;
    if !out.status.success() {
        anyhow::bail!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn current_period() -> String {
    chrono::Utc::now().format("%Y-%m").to_string()
}

/// Receipt text via poppler's `pdftotext`, which is already how koi reads PDFs
/// elsewhere on this host. A missing binary is reported once, not per file.
fn receipt_text(path: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("pdftotext")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn run_git_gc(apply: bool, threshold: u64) -> Result<()> {
    use koi_core::cleaners::git_objects::{collect, survey, RepoVerdict};

    let home = koi_core::state::home_dir()?;
    let repos = koi_core::monitors::git::find_git_repos(&home.join("projects"), 3);
    if repos.is_empty() {
        println!(
            "No git repositories found under {}/projects.",
            home.display()
        );
        return Ok(());
    }

    let statuses = survey(&repos, threshold);
    let mut to_collect = Vec::new();
    let mut skipped = Vec::new();
    let mut total_loose = 0u64;

    for st in &statuses {
        total_loose += st.loose_objects;
        match st.verdict {
            RepoVerdict::Collect => to_collect.push(st),
            RepoVerdict::MidOperation(op) => skipped.push((st, op)),
            RepoVerdict::BelowThreshold => {}
        }
    }

    println!(
        "Surveyed {} repo(s) under ~/projects — {total_loose} loose object(s) in total.",
        statuses.len()
    );
    println!("Threshold: {threshold} loose objects.");
    println!();

    if to_collect.is_empty() && skipped.is_empty() {
        println!("Nothing above the threshold. No repo needs collecting.");
        return Ok(());
    }

    if !skipped.is_empty() {
        // Named rather than silently dropped: a repo skipped for a rebase is
        // information the operator wants, not noise.
        println!("Skipped — an operation is in flight:");
        for (st, op) in &skipped {
            println!(
                "  {:<44} {:>6} loose  ({op} in progress)",
                st.path.display().to_string(),
                st.loose_objects
            );
        }
        println!();
    }

    if to_collect.is_empty() {
        return Ok(());
    }

    println!("Above threshold:");
    for st in &to_collect {
        println!(
            "  {:<44} {:>6} loose",
            st.path.display().to_string(),
            st.loose_objects
        );
    }
    println!();

    if !apply {
        println!("DRY RUN — nothing changed. Re-run with --apply to collect these.");
        println!("  gc runs with --prune=2.days.ago; koi never prunes to now, because");
        println!("  another session can be mid-commit in a repo being collected.");
        return Ok(());
    }

    println!("Collecting…");
    for st in &to_collect {
        let before = st.loose_objects;
        match collect(&st.path) {
            Ok(true) => {
                let after =
                    koi_core::cleaners::git_objects::count_loose(&st.path).unwrap_or(before);
                println!(
                    "  {:<44} {before:>6} -> {after}",
                    st.path.display().to_string()
                );
            }
            Ok(false) | Err(_) => {
                println!(
                    "  {:<44} gc FAILED — left alone",
                    st.path.display().to_string()
                );
            }
        }
    }
    Ok(())
}

fn run_fleet() -> Result<()> {
    use koi_core::fleet::{current_hostname, FleetConfig};

    let config = FleetConfig::load();
    let path = FleetConfig::default_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "~/.config/koi/fleet.toml".to_string());

    if config.machines.is_empty() {
        println!("No fleet declared ({path} is absent or empty).");
        println!();
        println!("A fleet config says which machines you run, what they should share,");
        println!("and what they are deliberately allowed to differ on.");
        println!("An annotated example ships at share/examples/fleet.toml.");
        return Ok(());
    }

    let Some(host) = current_hostname() else {
        println!("Fleet declared, but this machine's hostname could not be read.");
        return Ok(());
    };

    println!("Fleet ({} machine(s) declared)", config.machines.len());
    println!();

    match config.machine(&host) {
        None => {
            // Not an error: a machine may legitimately be outside the fleet.
            println!("This machine ({host}) is NOT declared in the fleet.");
            println!("koi will not compare it against anything.");
        }
        Some(me) => {
            let label = me.label.as_deref().unwrap_or("-");
            println!("This machine: {host}  ({}, {label})", me.os);

            let classes = config.classes_for(&host);
            if classes.is_empty() {
                println!("  Equivalence classes: none — nothing is expected to match.");
            } else {
                println!("  Equivalence classes:");
                for c in &classes {
                    println!(
                        "    {:<12} {:<14} with {}",
                        c.name,
                        c.kind,
                        c.machines.join(", ")
                    );
                }
            }

            let peers = config.peers_of(&host);
            if !peers.is_empty() {
                println!("  Peers: {}", peers.join(", "));
            }

            let divergences = config.divergences_for(&host);
            if divergences.is_empty() {
                println!("  Declared divergences: none.");
            } else {
                println!("  Declared divergences (koi will never propose to change these):");
                for (key, reason) in &divergences {
                    println!("    {key} — {}", reason.unwrap_or("no reason recorded"));
                }
            }
        }
    }

    println!();
    println!(
        "  Comparison and proposals are TASK-KOI159; this command only reports the declaration."
    );
    Ok(())
}

fn run_costs(action: CostsAction) -> Result<()> {
    use koi_core::subscriptions::{
        candidate_from_receipts, monthly_totals, renewals_within, Cadence, Register,
    };

    match action {
        CostsAction::Seed { dir } => {
            if which_pdftotext().is_none() {
                anyhow::bail!(
                    "pdftotext not found — install poppler-utils, or add rows by hand to \
                     ~/.config/koi/subscriptions.toml"
                );
            }
            let home = koi_core::state::home_dir()?;
            let dirs = if dir.is_empty() {
                vec![
                    home.join("Documents/PDFs-Inbox"),
                    home.join("Documents/PDFs"),
                    home.join("inbox"),
                ]
            } else {
                dir
            };

            // Group facts by provider before inferring anything: cadence is a
            // property of a provider's series, never of one receipt.
            let mut by_provider: std::collections::BTreeMap<
                String,
                Vec<koi_core::subscriptions::ReceiptFacts>,
            > = std::collections::BTreeMap::new();
            let mut read = 0usize;

            for d in &dirs {
                let Ok(entries) = std::fs::read_dir(d) else {
                    continue;
                };
                for entry in entries.filter_map(std::result::Result::ok) {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
                        continue;
                    }
                    read += 1;
                    if let Some(facts) = receipt_text(&path)
                        .and_then(|t| koi_core::subscriptions::parse_receipt_text(&t))
                    {
                        by_provider
                            .entry(facts.provider.clone())
                            .or_default()
                            .push(facts);
                    }
                }
            }

            let candidates: Vec<_> = by_provider
                .iter()
                .filter_map(|(provider, facts)| candidate_from_receipts(provider, facts))
                .collect();

            let mut register = Register::load();
            let added = register.merge_candidates(candidates.clone());
            register.save()?;

            println!(
                "Read {read} PDF(s); recognised {} provider(s).",
                by_provider.len()
            );
            for c in &candidates {
                let cadence = match c.cadence {
                    Cadence::Monthly => "monthly",
                    Cadence::Yearly => "yearly",
                    Cadence::OneOff => "one-off",
                };
                println!(
                    "  {:<14} {:>8.2} {}  {cadence:<8} from {}",
                    c.provider, c.amount, c.currency, c.source
                );
            }
            println!();
            println!(
                "{added} new row(s) written to ~/.config/koi/subscriptions.toml, unconfirmed."
            );
            println!("Nothing counts toward a total until you run `koi costs confirm <provider>`.");
        }

        CostsAction::Confirm { provider } => {
            let mut register = Register::load();
            let Some(row) = register
                .subscriptions
                .iter_mut()
                .find(|s| s.provider.eq_ignore_ascii_case(&provider))
            else {
                anyhow::bail!("no register row for {provider} — run `koi costs seed` first");
            };
            row.confirmed = true;
            let name = row.provider.clone();
            register.save()?;
            println!("Confirmed {name}; it now counts toward the monthly total.");
        }

        CostsAction::List => {
            let register = Register::load();
            if register.subscriptions.is_empty() {
                println!("Register is empty. Run `koi costs seed` to propose rows from receipts.");
                return Ok(());
            }
            println!("Subscriptions");
            for s in &register.subscriptions {
                let cadence = match s.cadence {
                    Cadence::Monthly => "monthly",
                    Cadence::Yearly => "yearly",
                    Cadence::OneOff => "one-off",
                };
                let mark = if s.confirmed { " " } else { "?" };
                println!(
                    "{mark} {:<14} {:>8.2} {:<4} {cadence:<8} next {}",
                    s.provider,
                    s.amount,
                    s.currency,
                    s.next_renewal.as_deref().unwrap_or("-")
                );
            }
            println!();
            let totals = monthly_totals(&register.subscriptions);
            if totals.is_empty() {
                println!("Monthly equivalent: nothing confirmed yet.");
            } else {
                for (currency, total) in &totals {
                    println!("Monthly equivalent: {total:.2} {currency}");
                }
                // Deliberately no combined figure: converting needs a rate and
                // a date, and a confidently wrong total is worse than two.
            }
            let today = chrono::Local::now().date_naive();
            let due = renewals_within(&register.subscriptions, today, 7);
            if !due.is_empty() {
                println!();
                println!("Renewing within 7 days:");
                for s in due {
                    println!(
                        "  {} on {}",
                        s.provider,
                        s.next_renewal.as_deref().unwrap_or("-")
                    );
                }
            }
            println!();
            println!("  ? = seeded from a receipt, not yet confirmed; excluded from totals.");
        }
    }
    Ok(())
}

fn which_pdftotext() -> Option<std::path::PathBuf> {
    std::process::Command::new("pdftotext")
        .arg("-v")
        .output()
        .ok()
        .and_then(|o| {
            o.status
                .success()
                .then(|| std::path::PathBuf::from("pdftotext"))
        })
}

fn run_cost(refresh: bool, json: bool) -> Result<()> {
    use koi_core::cost::{self, CostBudgets};

    let period = current_period();

    if refresh {
        let mut taken = 0usize;

        match read_cost_secret("railway-token") {
            None => println!("railway: no token at ~/.config/koi/secrets/railway-token — skipped."),
            Some(token) => {
                let query =
                    r#"{"query":"query { usage { projects { name estimatedCostCents } } }"}"#;
                match http_post_json("https://backboard.railway.com/graphql/v2", &token, query)
                    .and_then(|body| {
                        cost::parse_railway_usage(&body, &period).map_err(anyhow::Error::msg)
                    }) {
                    Ok(snaps) => {
                        let conn = open_state()?;
                        for s in &snaps {
                            koi_core::state::record_cost_snapshot(&conn, s)?;
                            taken += 1;
                        }
                    }
                    // A surface that cannot be read is reported and skipped:
                    // one broken token must not stop the other surface.
                    Err(e) => println!("railway: {e}"),
                }
            }
        }

        match (read_cost_secret("github-token"), read_cost_secret("github-account")) {
            (Some(token), Some(account)) => {
                let url = format!(
                    "https://api.github.com/users/{account}/settings/billing/actions"
                );
                match http_get_json(&url, &token).and_then(|body| {
                    cost::parse_github_actions_billing(&body, &account, &period)
                        .map_err(anyhow::Error::msg)
                }) {
                    Ok(snap) => {
                        let conn = open_state()?;
                        koi_core::state::record_cost_snapshot(&conn, &snap)?;
                        taken += 1;
                    }
                    Err(e) => println!("github-actions: {e}"),
                }
            }
            _ => println!(
                "github-actions: needs ~/.config/koi/secrets/github-token and github-account — skipped."
            ),
        }

        println!("Recorded {taken} snapshot(s) for {period}.");
        println!();
    }

    let conn = open_state()?;
    let snapshots = koi_core::state::latest_cost_snapshots(&conn)?;
    let budgets = CostBudgets::load();

    if json {
        println!("{}", serde_json::to_string_pretty(&snapshots)?);
        return Ok(());
    }

    if snapshots.is_empty() {
        println!("No cost snapshots recorded yet. Run `koi cost --refresh`.");
        return Ok(());
    }

    println!("Cost — month to date");
    let now = chrono::Utc::now();
    for s in &snapshots {
        let budget = budgets.for_provider(&s.provider);
        let flag = match cost::classify_budget(s.amount, budget) {
            cost::BudgetState::Over => "OVER",
            cost::BudgetState::Within => "ok",
            cost::BudgetState::Unset => "-",
        };
        let stale = if cost::is_stale(s.captured_at, now) {
            "  (stale)"
        } else {
            ""
        };
        println!(
            "  [{flag:>4}] {:<16} {:<20} {:>8.2} {}{stale}",
            s.provider, s.project, s.amount, s.currency
        );
    }
    println!();
    println!("  GitHub figures count PAID minutes only — a public repo is billed nothing.");
    Ok(())
}

fn run_zones(custom_roots: Vec<std::path::PathBuf>) -> Result<()> {
    use koi_core::filing::managed_zone;

    let roots = if !custom_roots.is_empty() {
        custom_roots
    } else {
        let home = koi_core::state::home_dir()?;
        vec![
            home.join("Documents"),
            home.join("Downloads"),
            home.join("inbox"),
            home.join("Desktop"),
        ]
    };

    let mut zones: Vec<managed_zone::ManagedZone> = Vec::new();
    for root in &roots {
        if !root.exists() {
            continue;
        }
        // Check the root itself and one level of children.
        if let Some(z) = managed_zone::load_zone(root) {
            zones.push(z);
        }
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.filter_map(|e| e.ok()) {
                let p = entry.path();
                if p.is_dir() {
                    if let Some(z) = managed_zone::load_zone(&p) {
                        zones.push(z);
                    }
                }
            }
        }
    }

    // The taxonomy destinations are koi's own claimed space. `koi zones` is
    // where an operator asks "what does koi consider spoken for?", so the
    // answer has to include them, not only other systems' markers
    // (TASK-KOI245 AC-3).
    let filing_cfg = FilingConfig::load();
    if let Ok(tax_root) = filing_cfg.documents_root() {
        println!(
            "Taxonomy destinations (koi's own, under {}):",
            tax_root.display()
        );
        for (destination, description) in filing_cfg.taxonomy.entries() {
            let marker = if tax_root.join(destination).is_dir() {
                "present"
            } else {
                "pending"
            };
            println!("  [{marker}] {destination} - {description}");
        }
        println!();
    }

    if zones.is_empty() {
        println!("No managed zones found in {} root(s).", roots.len());
        println!();
        println!("Drop a .koi-managed-by file in any directory to claim it:");
        println!("  system = \"the-book\"");
        println!("  scope = \"recursive\"");
        return Ok(());
    }

    println!("Managed zones ({}):", zones.len());
    for z in &zones {
        println!("  {}", z.root.display());
        println!("    system: {}", z.system);
        if let Some(owner) = &z.owner {
            println!("    owner:  {owner}");
        }
        println!("    scope:  {:?}", z.scope);
        if let Some(contact) = &z.contact {
            println!("    contact: {contact}");
        }
    }
    Ok(())
}

fn run_dedupe(action: DedupeAction) -> Result<()> {
    match action {
        DedupeAction::Scan { root } => run_dedupe_scan(root),
        DedupeAction::Apply {
            group,
            all_groups,
            dry_run,
        } => run_dedupe_apply(group, all_groups, dry_run),
    }
}

fn run_dedupe_scan(custom_roots: Vec<std::path::PathBuf>) -> Result<()> {
    let filing_cfg = FilingConfig::load();

    let roots = if !custom_roots.is_empty() {
        custom_roots
    } else {
        let home = koi_core::state::home_dir()?;
        vec![
            filing_cfg
                .roots
                .downloads
                .clone()
                .unwrap_or_else(|| home.join("Downloads")),
            filing_cfg
                .roots
                .documents
                .clone()
                .unwrap_or_else(|| home.join("Documents")),
            filing_cfg
                .roots
                .inbox
                .clone()
                .unwrap_or_else(|| home.join("inbox")),
        ]
    };
    let roots: Vec<_> = roots.into_iter().filter(|r| r.exists()).collect();
    if roots.is_empty() {
        println!("No existing roots to scan.");
        return Ok(());
    }

    let max_size_bytes = filing_cfg.dedupe.max_size_mb * 1024 * 1024;
    let groups = dedupe::scan(&roots, max_size_bytes);

    let conn = open_state()?;
    let now = chrono::Utc::now();
    for group in &groups {
        state::upsert_duplicate_group(&conn, group, now).context("persist duplicate group")?;
    }

    if groups.is_empty() {
        println!(
            "No duplicate groups found across {} root(s) (max size {}).",
            roots.len(),
            human(max_size_bytes)
        );
        return Ok(());
    }

    let reclaimable = dedupe::reclaimable_bytes(&groups);
    println!(
        "{} duplicate group(s) found, {} reclaimable if every non-keeper is trashed:",
        groups.len(),
        human(reclaimable)
    );
    for group in &groups {
        println!(
            "  [{}] {} × {} — keep {}",
            &group.content_hash[..8],
            group.members.len(),
            human(group.size),
            group.keeper.display()
        );
        for member in &group.members {
            let marker = if member.path == group.keeper {
                "keep"
            } else {
                "dupe"
            };
            println!("    {marker}  {}", member.path.display());
        }
    }
    Ok(())
}

fn run_dedupe_apply(group_prefix: Option<String>, all_groups: bool, dry_run: bool) -> Result<()> {
    if !all_groups && group_prefix.is_none() {
        anyhow::bail!("usage: koi dedupe apply --all-groups | koi dedupe apply <group-id-prefix>");
    }

    let conn = open_state()?;
    let groups = state::list_duplicate_groups(&conn).context("load duplicate groups")?;
    let matching: Vec<_> = if all_groups {
        groups
    } else {
        let prefix = group_prefix.expect("checked above");
        groups
            .into_iter()
            .filter(|g| g.group_id.starts_with(&prefix))
            .collect()
    };

    if matching.is_empty() {
        println!("No matching duplicate groups. Run `koi dedupe scan` first.");
        return Ok(());
    }

    let non_keepers: Vec<_> = matching
        .iter()
        .flat_map(|g| g.members.iter().filter(|m| !m.keep))
        .collect();

    if dry_run {
        println!(
            "DRY RUN — {} file(s) across {} group(s) would move to trash:",
            non_keepers.len(),
            matching.len()
        );
        for m in &non_keepers {
            println!("  {}", m.path.display());
        }
        return Ok(());
    }

    let home = koi_core::state::home_dir()?;
    let trash_root = trash::default_trash_root()?;
    let now = chrono::Utc::now();

    let mut moved = 0usize;
    let mut skipped = 0usize;
    for m in &non_keepers {
        match trash::move_to_trash(&m.path, &trash_root, &home, now) {
            Ok(trash_path) => {
                state::record_trash(&conn, &m.path, &trash_path, now)
                    .context("record trash entry")?;
                println!("  trashed  {}", m.path.display());
                moved += 1;
            }
            Err(e) => {
                println!("  skipped  {} ({e})", m.path.display());
                skipped += 1;
            }
        }
    }
    println!("{moved} moved to trash, {skipped} skipped.");
    Ok(())
}

fn run_trash(action: TrashAction) -> Result<()> {
    match action {
        TrashAction::List => run_trash_list(),
        TrashAction::Restore { id } => run_trash_restore(id),
        TrashAction::Empty { older_than, yes } => run_trash_empty(older_than, yes),
    }
}

fn run_trash_list() -> Result<()> {
    let conn = open_state()?;
    let entries = state::list_trash(&conn).context("list trash")?;
    if entries.is_empty() {
        println!("Trash is empty.");
        return Ok(());
    }
    println!("{} entry(ies) in trash:", entries.len());
    for e in &entries {
        println!(
            "  [{}] {}  (trashed {})",
            e.id,
            e.original_path.display(),
            e.trashed_at.to_rfc3339()
        );
    }
    Ok(())
}

fn run_trash_restore(id: i64) -> Result<()> {
    let conn = open_state()?;
    let entries = state::list_trash(&conn).context("list trash")?;
    let Some(entry) = entries.into_iter().find(|e| e.id == id) else {
        anyhow::bail!("no trash entry with id {id} (or it was already restored)");
    };
    trash::restore_from_trash(&entry.trash_path, &entry.original_path)
        .context("restore from trash")?;
    state::mark_restored(&conn, id, chrono::Utc::now()).context("mark restored")?;
    println!("Restored {}", entry.original_path.display());
    Ok(())
}

fn run_trash_empty(older_than: Option<String>, yes: bool) -> Result<()> {
    let older_than = match older_than {
        Some(s) => s,
        None => {
            let cfg = FilingConfig::load();
            format!("{}d", cfg.trash.retention_days)
        }
    };
    let window = trash::parse_older_than(&older_than).context("parse --older-than")?;
    let cutoff = chrono::Utc::now() - window;

    let conn = open_state()?;
    let candidates = state::trash_entries_older_than(&conn, cutoff).context("list candidates")?;

    if candidates.is_empty() {
        println!("Nothing older than {older_than} in trash.");
        return Ok(());
    }

    if !yes {
        println!(
            "{} entry(ies) older than {older_than} would be permanently deleted:",
            candidates.len()
        );
        for e in &candidates {
            println!("  [{}] {}", e.id, e.original_path.display());
        }
        println!("Re-run with --yes to actually delete. Nothing was removed.");
        return Ok(());
    }

    let mut deleted = 0usize;
    let mut failed = 0usize;
    for e in &candidates {
        match std::fs::remove_file(&e.trash_path) {
            Ok(()) => {
                // Reusing mark_restored/restored_at here: the column means
                // "no longer an active trash item", which covers both a
                // restore and a permanent deletion — trash_log has no
                // separate purged_at per ADR-0021's schema.
                state::mark_restored(&conn, e.id, chrono::Utc::now())
                    .context("mark trash entry cleared")?;
                deleted += 1;
            }
            Err(err) => {
                println!("  failed to delete {}: {err}", e.trash_path.display());
                failed += 1;
            }
        }
    }
    println!("{deleted} permanently deleted, {failed} failed.");
    Ok(())
}

fn run_clean(dry_run: bool) -> Result<()> {
    let home = koi_core::state::home_dir()?;
    let plan = cleaners::plan(&home);

    let mut total_bytes: u64 = 0;
    let existing: Vec<_> = plan
        .into_iter()
        .filter(|(_, _, existed, _)| *existed)
        .collect();

    // Exclude anything a running process still has open — the check that
    // made removing a 7.3GB unreferenced HuggingFace cache safe rather than
    // hopeful (WORK-KOI041). Named explicitly rather than silently skipped.
    let mut to_clean: Vec<_> = Vec::new();
    for entry in existing {
        if cleaners::path_has_live_reference(&entry.1) {
            println!(
                "  skipped: {} — a running process still has {} open",
                entry.0.name,
                entry.1.display()
            );
        } else {
            to_clean.push(entry);
        }
    }
    to_clean.sort_by(|a, b| b.3.cmp(&a.3));

    if to_clean.is_empty() {
        println!("Nothing to clean — all safe cache targets are already absent.");
        print_clean_proposals();
        return Ok(());
    }

    let action = if dry_run { "Would clean" } else { "Cleaning" };
    println!("{action}:");
    for (target, path, _, size) in &to_clean {
        println!(
            "  {:>8}  {:<25} {:<60} — {}",
            human(*size),
            target.name,
            path.display(),
            target.note
        );
        total_bytes += size;
    }
    println!(
        "\nTotal: {} across {} target(s)",
        human(total_bytes),
        to_clean.len()
    );

    if dry_run {
        println!("\nRe-run without --dry-run to apply.");
        print_clean_proposals();
        return Ok(());
    }

    let mut freed = 0u64;
    let mut cleaned = Vec::<String>::new();
    let mut failed = Vec::<(String, String)>::new();
    for (target, path, _, size) in &to_clean {
        match cleaners::execute_target(path) {
            Ok(()) => {
                freed += size;
                cleaned.push(target.name.to_string());
                println!("  ✓ {}", target.name);
            }
            Err(e) => {
                failed.push((target.name.to_string(), e.to_string()));
                println!("  ✗ {} ({e})", target.name);
            }
        }
    }
    println!("\nFreed: {}", human(freed));
    if !failed.is_empty() {
        println!("Failures: {}", failed.len());
    }

    if let Err(e) = record_clean_worklog(freed, &cleaned, &failed) {
        eprintln!("  (worklog not recorded: {e})");
    }

    print_clean_proposals();
    Ok(())
}

/// Build the worklog title and change lines for one `koi clean` run — pulled
/// out of `run_clean` so it can be tested without touching the filesystem.
fn clean_worklog_summary(
    freed: u64,
    cleaned: &[String],
    failed: &[(String, String)],
) -> (String, Vec<String>) {
    let title = if failed.is_empty() {
        format!(
            "koi clean freed {} across {} target(s)",
            human(freed),
            cleaned.len()
        )
    } else {
        format!(
            "koi clean freed {} across {} target(s), {} failed",
            human(freed),
            cleaned.len(),
            failed.len()
        )
    };
    let mut changes: Vec<String> = cleaned
        .iter()
        .map(|name| format!("cleared {name}"))
        .collect();
    changes.extend(
        failed
            .iter()
            .map(|(name, err)| format!("failed: {name} ({err})")),
    );
    (title, changes)
}

/// Record one executed `koi clean` run to the shared worklog (TASK-KOI190) so
/// non-dry-run cleanup is auditable the same way manual sweeps already are.
fn record_clean_worklog(freed: u64, cleaned: &[String], failed: &[(String, String)]) -> Result<()> {
    let (title, changes) = clean_worklog_summary(freed, cleaned, failed);
    let path = koi_core::worklog::worklog_path()?;
    koi_core::worklog::append(&path, "koi-cli", &title, None, "maintenance", changes)?;
    Ok(())
}

/// Propose-only cleanup candidates — printed alongside `koi clean`'s
/// auto-executed section but NEVER acted on here. Snap revisions need root;
/// docker volumes can hold real data; both are the propose-and-wait tier.
fn print_clean_proposals() {
    let mut proposals = cleaners::docker_dangling_volume_proposals();
    proposals.extend(cleaners::snap_disabled_revision_proposals());

    if proposals.is_empty() {
        return;
    }

    println!("\nProposals (review before acting — not auto-executed):");
    for p in &proposals {
        println!("  [{}] {}", p.kind, p.description);
        println!("      {}", p.command_hint);
    }
}

fn human(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b >= K * K * K {
        format!("{:.1}G", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.1}M", b / (K * K))
    } else if b >= K {
        format!("{:.1}K", b / K)
    } else {
        format!("{}B", bytes)
    }
}

// An exclude pattern like "**/target/" or "**/.git/objects/" names one or
// more trailing path components to match anywhere under a walked root
// (leading "**/" and trailing "/" are structural, not literal glob text).
// A path is excluded if that component sequence appears consecutively
// anywhere in its components relative to the root.
fn exclude_components(pattern: &str) -> Vec<glob::Pattern> {
    let trimmed = pattern
        .strip_prefix("**/")
        .unwrap_or(pattern)
        .trim_end_matches('/');
    trimmed
        .split('/')
        .filter(|s| !s.is_empty())
        .filter_map(|s| glob::Pattern::new(s).ok())
        .collect()
}

fn is_excluded(rel_components: &[&std::ffi::OsStr], excludes: &[Vec<glob::Pattern>]) -> bool {
    excludes.iter().any(|pat_seq| {
        if pat_seq.is_empty() || pat_seq.len() > rel_components.len() {
            return false;
        }
        (0..=rel_components.len() - pat_seq.len()).any(|start| {
            pat_seq.iter().enumerate().all(|(i, pat)| {
                rel_components[start + i]
                    .to_str()
                    .is_some_and(|c| pat.matches(c))
            })
        })
    })
}

// Discover git repositories sitting directly inside `scan_rel` (a path relative
// to `root`) and return one backup.toml-shaped exclude pattern per repo, sorted
// so the argv and the local walk stay byte-identical between runs.
//
// Why this exists: a reference-material tree holds loose material that has no
// other copy and must be backed up, alongside git repos that already have their
// own remote and are duplicate protection to upload again. The static per-repo
// exclude list this replaces needed a hand edit every time a repo was added,
// and was silently wrong — a whole repo re-uploaded — whenever that was
// forgotten (TASK-KOI166).
//
// `.git` is probed with `exists()` rather than `is_dir()` on purpose: a
// submodule or a linked worktree carries `.git` as a FILE, and treating those
// as non-repos would re-include exactly the trees this is meant to skip.
fn git_repo_excludes_under(root: &std::path::Path, scan_rel: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join(scan_rel)) else {
        // A configured tree that does not exist on this machine is not an
        // error: the same backup.toml is meant to work across hosts.
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join(".git").exists())
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .collect();
    names.sort();
    let prefix = scan_rel.trim_end_matches('/');
    names
        .into_iter()
        .map(|n| format!("{prefix}/{n}/"))
        .collect()
}

// Combine the hand-written exclude patterns from backup.toml with the ones
// discovered by walking each configured auto-exclude tree, preserving the
// static entries' order and dropping any duplicate the two sources both name.
//
// The dedupe is what lets the hand-maintained per-repo lines be deleted
// safely: until they are, both sources name the same repo, and emitting it
// twice would put a duplicate --exclude on the rclone argv (TASK-KOI166 AC-3).
fn merged_exclude_patterns(
    static_patterns: &[String],
    roots: &[std::path::PathBuf],
    scan_rels: &[String],
) -> Vec<String> {
    let mut merged: Vec<String> = static_patterns.to_vec();
    for root in roots {
        for scan_rel in scan_rels {
            for pattern in git_repo_excludes_under(root, scan_rel) {
                if !merged.contains(&pattern) {
                    merged.push(pattern);
                }
            }
        }
    }
    merged
}

// Measure how many bytes the encrypted remote currently holds and record that
// against the local filtered total. `rclone size` on a crypt remote reports
// decrypted sizes, so the two totals are directly comparable.
//
// This is the completion signal that replaces "one systemd run exited 0"
// (TASK-KOI192): on a workstation that reboots several times a day, no single
// run of a multi-day sync ever ends cleanly, but rclone resumes and progress
// accrues on the remote regardless.
fn measure_convergence(
    local_bytes: u64,
) -> Result<koi_core::backup_convergence::ConvergenceSnapshot> {
    use koi_core::backup_convergence::{
        is_rate_limited, parse_rclone_size_bytes, ConvergenceSnapshot,
    };

    // --fast-list is what makes this viable: it walks the remote recursively in
    // a few large listings instead of one API call per directory. --tpslimit
    // then paces those few calls so the query does not exhaust the Drive quota
    // when a sync is running and both draw on it (observed 2026-07-27).
    //
    // The two flags are load-bearing *together*. Throttling to 4 tps without
    // --fast-list throttles a per-directory walk and the measurement crawls —
    // 23 minutes without finishing on this tree before --fast-list was added.
    let output = std::process::Command::new("rclone")
        .args([
            "size",
            "koi-crypt:/",
            "--json",
            "--fast-list",
            "--tpslimit",
            "4",
            "--retries",
            "3",
        ])
        .output()
        .context("Failed to run rclone size — is rclone installed and koi-crypt configured?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_rate_limited(&stderr) {
            anyhow::bail!(
                "Remote is rate-limiting the size query — this usually means a backup sync is \
                 already running and both are drawing on the same Google Drive quota. \
                 Convergence is unchanged; re-measure once the sync is idle."
            );
        }
        anyhow::bail!("rclone size failed:\n{stderr}");
    }
    let remote_bytes = parse_rclone_size_bytes(&String::from_utf8_lossy(&output.stdout))
        .context("Could not read byte total from rclone size --json")?;
    Ok(ConvergenceSnapshot::new(
        local_bytes,
        remote_bytes,
        chrono::Utc::now(),
    ))
}

fn report_convergence(local_bytes: u64) -> Result<()> {
    use koi_core::backup_convergence::write_snapshot;

    println!("\nMeasuring remote convergence (rclone size koi-crypt:/)...");
    let snapshot = measure_convergence(local_bytes)?;
    write_snapshot(&snapshot).context("Failed to persist convergence snapshot")?;

    println!("  Local (filtered): {}", human(snapshot.local_bytes));
    println!("  Remote:           {}", human(snapshot.remote_bytes));
    println!("  Converged:        {}%", snapshot.percent());
    if snapshot.converged {
        println!("\n✓ Remote has converged on the local source.");
    } else {
        println!("\nStill converging — the next run resumes where this left off.");
    }
    Ok(())
}

/// Transactions per second the backup sync may issue against Drive.
///
/// Sized against the real failure rather than picked: `measure_convergence` uses
/// 4, which suits a short read but would stall a sync of this tree indefinitely.
/// 10/s is Drive's documented per-user steady-state shape, and it is the pacing —
/// not the ceiling — that stops the 403 in INC-KOI023.
const RCLONE_SYNC_TPS_LIMIT: u32 = 10;

/// Burst allowance above `RCLONE_SYNC_TPS_LIMIT`, so a directory listing is not
/// artificially serialised while the average stays inside quota.
const RCLONE_SYNC_TPS_BURST: u32 = 20;

/// High-level retries. A sync that trips quota should back off and resume rather
/// than exit non-zero and wait a week for the timer (INC-KOI023: six consecutive
/// runs failed outright, each transferring 0 B).
const RCLONE_SYNC_RETRIES: u32 = 5;

/// Per-object retries for transient 403/429 responses, which Drive returns
/// routinely under contention and which should not fail an entire run.
const RCLONE_SYNC_LOW_LEVEL_RETRIES: u32 = 20;

fn run_backup(dry_run: bool, include_red: bool, status: bool) -> Result<()> {
    use std::path::PathBuf;

    let home = koi_core::state::home_dir()?;

    // Load backup config
    let config_path = home.join(".config/koi/backup.toml");
    if !config_path.exists() {
        anyhow::bail!("Backup config not found: {}", config_path.display());
    }
    let config_text =
        std::fs::read_to_string(&config_path).context("Failed to read backup config")?;
    let config: toml::Table =
        toml::from_str(&config_text).context("Failed to parse backup.toml")?;

    // Helper: collect every file under a configured root, attributing each to
    // the tier that listed it. Tier membership is taken from the config, never
    // inferred from directory names — no personal layout is baked in.
    fn collect_tier(
        roots: &[PathBuf],
        excludes: &[Vec<glob::Pattern>],
        to_upload: &mut Vec<(PathBuf, u64)>,
    ) -> (usize, u64) {
        let mut count = 0usize;
        let mut bytes = 0u64;
        for path in roots {
            if !path.exists() {
                continue;
            }
            if path.is_file() {
                if let Ok(metadata) = path.metadata() {
                    to_upload.push((path.clone(), metadata.len()));
                    count += 1;
                    bytes += metadata.len();
                }
            } else if path.is_dir() {
                for entry in walkdir::WalkDir::new(path)
                    .into_iter()
                    .filter_entry(|e| {
                        let rel: Vec<&std::ffi::OsStr> = e
                            .path()
                            .strip_prefix(path)
                            .map(|r| r.components().map(|c| c.as_os_str()).collect())
                            .unwrap_or_default();
                        !is_excluded(&rel, excludes)
                    })
                    .filter_map(|e| e.ok())
                {
                    if entry.path().is_file() {
                        if let Ok(metadata) = entry.metadata() {
                            to_upload.push((entry.path().to_path_buf(), metadata.len()));
                            count += 1;
                            bytes += metadata.len();
                        }
                    }
                }
            }
        }
        (count, bytes)
    }

    let read_roots = |table_key: &str, array_key: &str| -> Vec<PathBuf> {
        config
            .get(table_key)
            .and_then(|v| v.as_table())
            .and_then(|t| t.get(array_key))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str())
                    .map(|pattern| PathBuf::from(shellexpand::tilde(pattern).into_owned()))
                    .collect()
            })
            .unwrap_or_default()
    };

    let read_strings = |table_key: &str, array_key: &str| -> Vec<String> {
        config
            .get(table_key)
            .and_then(|v| v.as_table())
            .and_then(|t| t.get(array_key))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };

    let amber_roots = read_roots("amber_tier", "include");
    let red_roots = if include_red {
        read_roots("red_tier", "paths")
    } else {
        Vec::new()
    };

    // One exclude list, derived once, used by both consumers: the local walk
    // just below (via exclude_components) and the rclone argv further down (via
    // rclone_exclude_arg). Before TASK-KOI166 each read the config key
    // independently, so any divergence between them would have meant the set of
    // files koi measures and the set rclone uploads were quietly different sets.
    //
    // `auto_exclude_git_repos_under` names trees whose immediate subdirectories
    // are checked for their own `.git` at scan time. A repo found there is
    // excluded without anyone editing backup.toml, which is the whole point: the
    // static list it replaces was one forgotten edit away from re-uploading an
    // entire repo that already has a remote.
    let exclude_patterns = merged_exclude_patterns(
        &read_strings("amber_tier", "exclude"),
        &amber_roots,
        &read_strings("amber_tier", "auto_exclude_git_repos_under"),
    );

    let amber_excludes: Vec<Vec<glob::Pattern>> = exclude_patterns
        .iter()
        .map(|p| exclude_components(p))
        .collect();

    // Collect files to upload, tracking each tier's count as we go.
    let mut to_upload: Vec<(PathBuf, u64)> = Vec::new();
    let (amber_count, amber_bytes) = collect_tier(&amber_roots, &amber_excludes, &mut to_upload);
    let (red_count, red_bytes) = collect_tier(&red_roots, &[], &mut to_upload);
    let total_bytes = amber_bytes + red_bytes;

    if to_upload.is_empty() {
        println!("No files to backup.");
        return Ok(());
    }

    println!("Files to backup:");
    println!("  Amber tier: {} files", amber_count);
    if include_red {
        println!("  Red tier:   {} files", red_count);
    }
    println!("  Total:      {} ({})", to_upload.len(), human(total_bytes));

    // --status measures and reports only; it never syncs.
    if status {
        return report_convergence(total_bytes);
    }

    if dry_run {
        println!("\nDry run — no files uploaded.");
        println!("Run without --dry-run to sync to rclone remote 'koi-crypt'.");
        return Ok(());
    }

    // Execute one rclone sync per configured root, each to its own
    // destination subpath (roots share one remote, so they can't all sync
    // to koi-crypt:/ directly — that would let the last root clobber the
    // first). KOI_BACKUP_SOURCE, if set, overrides with a single ad-hoc
    // root synced to the remote's top level.
    let sync_roots: Vec<(PathBuf, String)> =
        if let Some(src) = std::env::var_os("KOI_BACKUP_SOURCE") {
            vec![(PathBuf::from(src), String::new())]
        } else {
            let mut roots: Vec<(PathBuf, String)> = amber_roots
                .iter()
                .map(|r| (r.clone(), rclone_dest_name(r)))
                .collect();
            if include_red {
                roots.extend(red_roots.iter().map(|r| (r.clone(), rclone_dest_name(r))));
            }
            roots
        };
    if sync_roots.is_empty() {
        anyhow::bail!(
            "No backup source: set KOI_BACKUP_SOURCE or add an amber_tier.include entry to backup.toml"
        );
    }

    // Same merged list the local walk used, so the measured set and the
    // uploaded set cannot drift apart.
    let exclude_args: Vec<String> = exclude_patterns
        .iter()
        .map(|p| rclone_exclude_arg(p))
        .collect();

    for (root, dest_name) in &sync_roots {
        let dest = format!("koi-crypt:/{dest_name}");
        println!("\nStarting rclone sync {} -> {dest}...", root.display());
        // --fast-list fetches the remote tree in far fewer API round-trips
        // instead of walking it directory by directory. That re-walk is the
        // dominant restart cost here: a reboot kills the sync every few hours
        // and the next run re-checks the whole tree from cold (TASK-KOI192).
        //
        // The trade is memory — rclone holds the full listing in RAM, roughly
        // 1KB per object, so a few hundred MB at this tree size. Enabled on the
        // operator's explicit decision (2026-07-27) with the OOM history on this
        // host understood (INC-KOI009, INC-KOI013, INC-KOI016). If a backup run
        // is ever implicated in another OOM event, dropping this one flag is the
        // first thing to try.
        //
        // --tpslimit / --retries pace those listings. Without them this path
        // burst-listed a 746k-object tree flat out and Drive answered 403
        // rateLimitExceeded during the listing phase, before a single byte moved
        // — six consecutive runs transferred 0 B (INC-KOI023). measure_convergence
        // above already carried these flags from 776ce32; the sync path was
        // simply never given the same treatment. The limit is deliberately higher
        // than the measurement's 4: this path moves real data and a sync paced at
        // 4 tps would never finish, whereas the measurement is a short read.
        let mut args: Vec<String> = vec![
            "sync".into(),
            "--verbose".into(),
            "--progress".into(),
            "--fast-list".into(),
            "--tpslimit".into(),
            RCLONE_SYNC_TPS_LIMIT.to_string(),
            "--tpslimit-burst".into(),
            RCLONE_SYNC_TPS_BURST.to_string(),
            "--retries".into(),
            RCLONE_SYNC_RETRIES.to_string(),
            "--low-level-retries".into(),
            RCLONE_SYNC_LOW_LEVEL_RETRIES.to_string(),
        ];
        for pattern in &exclude_args {
            args.push("--exclude".into());
            args.push(pattern.clone());
        }
        args.push(root.to_str().unwrap_or(".").to_string());
        args.push(dest);

        let output = std::process::Command::new("rclone")
            .args(&args)
            .output()
            .with_context(|| format!("Failed to run rclone for {}", root.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("rclone sync failed for {}:\n{}", root.display(), stderr);
        }
    }

    println!("\n✓ Backup completed successfully.");
    println!("Synced: {}", human(total_bytes));

    // Record convergence so `koi check` can report it without a network call.
    // Best-effort: the sync genuinely succeeded, so a failed measurement must
    // not turn that into a failed run.
    if let Err(err) = report_convergence(total_bytes) {
        eprintln!("Warning: could not measure remote convergence: {err:#}");
    }
    Ok(())
}

// Destination subpath under koi-crypt:/ for a given source root. Uses the
// root's final path component (e.g. ~/projects -> "projects") so distinct
// roots land in distinct remote locations instead of overwriting each other.
fn rclone_dest_name(root: &std::path::Path) -> String {
    root.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("backup")
        .to_string()
}

// Convert a backup.toml exclude pattern ("**/target/", "**/.env") into an
// rclone filter pattern. A directory pattern (trailing "/") becomes
// "**/name/**" so rclone excludes everything beneath it; a file pattern
// passes through unchanged.
fn rclone_exclude_arg(pattern: &str) -> String {
    match pattern.strip_suffix('/') {
        Some(dir) => format!("{dir}/**"),
        None => pattern.to_string(),
    }
}

fn run_stats() -> Result<()> {
    let conn = open_state()?;

    println!("# Koi Stats");
    println!();

    // Proposals by state + monitor.
    let mut stmt = conn.prepare(
        "SELECT monitor, state, COUNT(*) FROM proposals GROUP BY monitor, state ORDER BY monitor, state"
    )?;
    let rows: Vec<(String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    if rows.is_empty() {
        println!("No proposals yet. Run `koi scan`.");
    } else {
        println!("## Proposals by monitor × state");
        println!();
        println!("| Monitor | State | Count |");
        println!("|---|---|---|");
        for (m, s, c) in &rows {
            println!("| {} | {} | {} |", m, s, c);
        }
    }

    // Decisions by kind.
    println!();
    println!("## Decisions");
    println!();
    let decisions: Vec<(String, i64)> = conn
        .prepare("SELECT decision, COUNT(*) FROM decisions GROUP BY decision")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    if decisions.is_empty() {
        println!("_No decisions yet. Run `koi approve` or `koi reject`._");
    } else {
        println!("| Decision | Count |");
        println!("|---|---|");
        for (d, c) in &decisions {
            println!("| {} | {} |", d, c);
        }
    }

    // Monitor report counts.
    println!();
    println!("## Monitor reports");
    println!();
    let reports: Vec<(String, i64, Option<String>)> = conn.prepare(
        "SELECT monitor, COUNT(*), MAX(collected_at) FROM monitor_reports GROUP BY monitor ORDER BY monitor"
    )?.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    if reports.is_empty() {
        println!("_No reports yet. Run `koi check` or start `koi-daemon`._");
    } else {
        println!("| Monitor | Total runs | Last seen |");
        println!("|---|---|---|");
        for (m, c, last) in &reports {
            println!("| {} | {} | {} |", m, c, last.as_deref().unwrap_or("—"));
        }
    }

    Ok(())
}

fn run_report(output: Option<std::path::PathBuf>) -> Result<()> {
    let conn = open_state()?;
    let monitors = [
        "DiskMonitor",
        "BackupMonitor",
        "MemoryMonitor",
        "ModelSizeMonitor",
        "CacheMonitor",
        "DockerMonitor",
        "GitMonitor",
        "PackageMonitor",
        "NetworkMonitor",
        "LatencyMonitor",
    ];
    let pending = state::pending_proposals(&conn).context("load pending proposals")?;

    use std::fmt::Write;
    let mut buf = String::new();
    let now = chrono::Utc::now();
    writeln!(buf, "# Koi Health Report").unwrap();
    writeln!(buf).unwrap();
    writeln!(buf, "Generated: {}", now.to_rfc3339()).unwrap();
    writeln!(buf).unwrap();

    writeln!(buf, "## Monitors").unwrap();
    writeln!(buf).unwrap();
    writeln!(
        buf,
        "| Monitor | Status | Last Run | Elapsed | Suggestions |"
    )
    .unwrap();
    writeln!(buf, "|---|---|---|---|---|").unwrap();
    for name in &monitors {
        let report = state::latest_monitor_report(&conn, name).ok().flatten();
        if let Some(r) = report {
            let icon = match r.status {
                HealthStatus::Healthy => "🟢 ok",
                HealthStatus::Warning => "🟡 warn",
                HealthStatus::Critical => "🔴 crit",
            };
            writeln!(
                buf,
                "| {} | {} | {} | {}ms | {} |",
                name,
                icon,
                r.collected_at.format("%Y-%m-%d %H:%M"),
                r.elapsed_ms,
                r.suggestions.len()
            )
            .unwrap();
        } else {
            writeln!(buf, "| {} | — | never run | — | — |", name).unwrap();
        }
    }

    writeln!(buf).unwrap();
    writeln!(buf, "## Suggestions (top per monitor)").unwrap();
    writeln!(buf).unwrap();
    for name in &monitors {
        if let Some(r) = state::latest_monitor_report(&conn, name).ok().flatten() {
            if !r.suggestions.is_empty() {
                writeln!(buf, "### {name}").unwrap();
                for s in r.suggestions.iter().take(3) {
                    writeln!(buf, "- {}", s.message).unwrap();
                }
                writeln!(buf).unwrap();
            }
        }
    }

    writeln!(buf, "## Pending Filing Proposals").unwrap();
    writeln!(buf).unwrap();
    if pending.is_empty() {
        writeln!(buf, "_No pending proposals._").unwrap();
    } else {
        let mut by_mon: std::collections::BTreeMap<String, Vec<_>> =
            std::collections::BTreeMap::new();
        for p in &pending {
            by_mon.entry(p.monitor.clone()).or_default().push(p);
        }
        writeln!(buf, "Total pending: **{}**", pending.len()).unwrap();
        writeln!(buf).unwrap();
        for (mon, items) in &by_mon {
            writeln!(buf, "- **{mon}**: {} proposal(s)", items.len()).unwrap();
        }
        writeln!(buf).unwrap();
        writeln!(buf, "Run `koi proposals` for detail, `koi approve --all` to apply, `koi reject <id>` to decline.").unwrap();
    }

    match output {
        None => print!("{buf}"),
        Some(path) => {
            let target = if path.is_dir() || path.to_string_lossy().ends_with('/') {
                std::fs::create_dir_all(&path).ok();
                path.join(format!("koi-report-{}.md", now.format("%Y-%m-%dT%H%M%SZ")))
            } else {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                path.clone()
            };
            std::fs::write(&target, &buf).with_context(|| format!("write {}", target.display()))?;
            eprintln!("Report written to {}", target.display());
        }
    }

    Ok(())
}

fn run_history(monitor: &str, limit: usize) -> Result<()> {
    // "audit" is a special case — it reads audit_runs, not monitor_reports.
    if monitor.to_ascii_lowercase().contains("audit") {
        return run_history_audit(limit);
    }
    let conn = open_state()?;
    let reports = state::recent_monitor_reports(&conn, monitor, limit).context("query history")?;
    if reports.is_empty() {
        println!("No history for {monitor}. Run `koi check` or start `koi-daemon` first.");
    } else {
        println!(
            "Last {} report(s) for {monitor} (newest first):",
            reports.len()
        );
        for r in &reports {
            let status = match r.status {
                HealthStatus::Healthy => "ok",
                HealthStatus::Warning => "warn",
                HealthStatus::Critical => "crit",
            };
            println!(
                "  {}  [{}] {} ms  {} observation(s), {} suggestion(s)",
                r.collected_at.to_rfc3339(),
                status,
                r.elapsed_ms,
                r.observations.len(),
                r.suggestions.len(),
            );
        }
    }

    // For terminal monitors, also list recent crash events.
    let crash_comm = if monitor.to_ascii_lowercase().contains("wezterm") {
        Some("wezterm-gui")
    } else if monitor.to_ascii_lowercase().contains("ghostty") {
        Some("ghostty")
    } else {
        None
    };

    if let Some(comm) = crash_comm {
        let label = if comm == "wezterm-gui" {
            "WezTerm"
        } else {
            "Ghostty"
        };
        let crashes =
            state::recent_process_crashes(&conn, comm, limit).context("query crash history")?;
        if crashes.is_empty() {
            println!("No {label} crashes recorded in the last {limit} events.");
        } else {
            println!("\nCrash events (newest first):");
            for c in &crashes {
                let pid_str = c.pid.map(|p| format!(" pid={p}")).unwrap_or_default();
                let rss_str = c
                    .last_rss_mb
                    .map(|mb| format!(" rss={mb:.0}MiB", mb = mb))
                    .unwrap_or_default();
                println!(
                    "  {}  [{}]{}{} {}",
                    c.detected_at.to_rfc3339(),
                    c.crash_type,
                    pid_str,
                    rss_str,
                    &c.message[..c.message.len().min(120)],
                );
            }
        }
    }
    Ok(())
}

fn run_history_audit(limit: usize) -> Result<()> {
    use koi_core::state::recent_audit_runs;
    let conn = open_state()?;
    let runs = recent_audit_runs(&conn, limit).context("query audit history")?;
    if runs.is_empty() {
        println!("No audit history. Run: sudo koi audit");
        return Ok(());
    }
    println!("Last {} audit run(s) (newest first):", runs.len());
    for r in &runs {
        let score = r
            .hardening_index
            .map(|s| format!("{s}/100"))
            .unwrap_or_else(|| "?/100".into());
        let kind = if r.quick { "quick" } else { "full" };
        println!(
            "  {}  [{}] {}  {}",
            r.ran_at.format("%Y-%m-%d %H:%M UTC"),
            kind,
            score,
            r.report_path
        );
    }
    Ok(())
}

fn open_state() -> Result<rusqlite::Connection> {
    let path = state::default_db_path().context("resolve DB path")?;
    state::open(&path).context("open SQLite state")
}

/// Print the taxonomy in one screen: destination, description, and whether the
/// directory exists yet. Read by `koi scan --explain` (TASK-KOI245 AC-4).
fn print_taxonomy(cfg: &FilingConfig, json: bool) -> Result<()> {
    if let Err(e) = cfg.taxonomy.validate() {
        // A taxonomy that does not validate is the operator's to fix, and
        // printing it as though it were fine would hide that.
        eprintln!("warning: taxonomy is not valid: {e}");
    }
    let root = cfg.documents_root().unwrap_or_default();

    if json {
        let entries: Vec<_> = cfg
            .taxonomy
            .entries()
            .map(|(d, desc)| {
                serde_json::json!({
                    "destination": d,
                    "description": desc,
                    "path": root.join(d),
                    "exists": root.join(d).is_dir(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "root": root,
                "destinations": entries,
            }))?
        );
        return Ok(());
    }

    println!("Filing taxonomy - what \"organised\" means on this machine");
    println!("Root: {}", root.display());
    println!();
    let width = cfg
        .taxonomy
        .entries()
        .map(|(d, _)| d.len())
        .max()
        .unwrap_or(0);
    for (destination, description) in cfg.taxonomy.entries() {
        let marker = if root.join(destination).is_dir() {
            " "
        } else {
            "+"
        };
        println!("{marker} {destination:<width$}  {description}");
    }
    println!();
    println!("  + = directory does not exist yet; `koi scan` creates it.");
    Ok(())
}

fn run_scan(json: bool, explain: bool) -> Result<()> {
    let filing_cfg = FilingConfig::load();

    // --explain answers "where would my files go, and why" without touching
    // anything. It is the screen the operator reads before approving rules
    // (TASK-KOI245 AC-4).
    if explain {
        return print_taxonomy(&filing_cfg, json);
    }

    // Taxonomy destinations are created up front and idempotently, so a rule
    // that routes to one cannot fail for want of a directory (AC-3).
    match filing_cfg.ensure_taxonomy_dirs() {
        Ok(report) => {
            if !report.created.is_empty() && !json {
                println!("Created {} taxonomy destination(s):", report.created.len());
                for p in &report.created {
                    println!("  {}", p.display());
                }
                println!();
            }
            if !report.managed.is_empty() && !json {
                println!(
                    "Skipped {} destination(s) inside a managed zone.",
                    report.managed.len()
                );
            }
        }
        // A taxonomy directory koi cannot create is worth saying out loud, but
        // it must not stop the scan: the extension rules still work.
        Err(e) => eprintln!("warning: could not create taxonomy destinations: {e}"),
    }
    let mut monitors: Vec<Box<dyn FileMonitor>> = vec![
        Box::new(DownloadsMonitor::from_config(&filing_cfg).context("DownloadsMonitor init")?),
        Box::new(DocumentsMonitor::from_config(&filing_cfg).context("DocumentsMonitor init")?),
        Box::new(InboxMonitor::from_config(&filing_cfg).context("InboxMonitor init")?),
        Box::new(RootClutterMonitor::from_config(&filing_cfg).context("RootClutterMonitor init")?),
    ];
    if let Some(gdrive) = GoogleDriveMonitor::load() {
        monitors.push(Box::new(gdrive));
    }

    // Build scan context with a classifier reading from the DB (learning loop).
    // Zone cache is discovered up front over every monitor's roots so managed
    // zones (.koi-managed-by) are honoured for all of them, not just whichever
    // monitor happens to build its own local cache.
    let roots: Vec<_> = monitors.iter().flat_map(|m| m.roots()).collect();
    let classifier_conn = open_state()?;
    let ctx = ScanContext::new_now_with_roots(&roots)
        .with_classifier(Box::new(SqliteClassifier::new(classifier_conn)));

    let mut all_proposals = Vec::new();
    for m in &monitors {
        let proposals = m.scan(&ctx).with_context(|| format!("{} scan", m.name()))?;
        all_proposals.extend(proposals);
    }

    // Persist proposals — idempotent upsert, safe to re-scan. Separate
    // write-connection from the classifier's read-connection.
    //
    // A proposal the operator already rejected is not re-queued (ADR-0014 makes
    // rejection a first-class signal), so it must not be reported as stored
    // either — that mismatch between scan output and `koi proposals` is the
    // defect TASK-KOI228 fixes.
    let conn = open_state()?;
    let mut queued: Vec<&koi_core::filing::Proposal> = Vec::new();
    let mut suppressed = 0usize;
    for p in &all_proposals {
        match state::upsert_proposal(&conn, p).context("persist proposal")? {
            state::UpsertOutcome::SuppressedByRejection => suppressed += 1,
            _ => queued.push(p),
        }
    }

    // Stale-proposal sweep: a pending proposal whose source has since
    // vanished (moved/deleted some other way) stops being retried forever.
    for m in &monitors {
        state::supersede_stale_proposals(&conn, m.name()).context("sweep stale proposals")?;
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&queued)?);
        return Ok(());
    }

    if queued.is_empty() {
        if suppressed > 0 {
            println!(
                "No new proposals — {} held back by an earlier rejection (`koi history decisions` to review).",
                suppressed
            );
        } else {
            println!(
                "No proposals — accumulation points are clean or contain only unknown-type files."
            );
        }
        return Ok(());
    }

    println!(
        "{} proposal(s) — stored; review with `koi proposals`, apply with `koi approve --all`:",
        queued.len()
    );
    for p in &queued {
        let action_str = match &p.action {
            ProposedAction::Move { dest } => format!("→ {}", dest.display()),
            ProposedAction::Archive { archive_root } => {
                format!("archive to {}", archive_root.display())
            }
            ProposedAction::Delete => "delete".into(),
            ProposedAction::Tag { labels } => format!("tag: {}", labels.join(", ")),
            ProposedAction::Ignore { reason } => format!("ignore ({reason})"),
            ProposedAction::DriveMove { remote_dest, .. } => {
                format!("drive → {remote_dest}")
            }
        };
        println!(
            "  [{} {:.0}%] {} {}\n         from {}",
            &p.id.0[..8],
            p.confidence * 100.0,
            p.rationale,
            action_str,
            p.path.display()
        );
    }
    Ok(())
}

fn run_proposals(monitor: Option<String>, limit: Option<usize>) -> Result<()> {
    let conn = open_state()?;
    let mut pending = state::pending_proposals(&conn).context("load pending")?;
    if let Some(mon) = &monitor {
        pending.retain(|p| &p.monitor == mon);
    }
    if let Some(n) = limit {
        pending.truncate(n);
    }
    if pending.is_empty() {
        println!("No pending proposals. Run `koi scan` first.");
        return Ok(());
    }

    // Summary header grouped by monitor.
    let mut by_mon: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for p in &pending {
        *by_mon.entry(p.monitor.as_str()).or_insert(0) += 1;
    }
    println!("{} pending proposal(s):", pending.len());
    for (mon, n) in &by_mon {
        println!("  {:<20} {}", mon, n);
    }
    println!();

    for p in &pending {
        println!(
            "  [{}] {} ({:.0}%) {} — {}",
            &p.id.0[..8],
            p.action_kind,
            p.confidence * 100.0,
            p.path.display(),
            p.rationale
        );
    }
    Ok(())
}

/// Reject an `<id-prefix>` argument that is not a usable hex prefix.
///
/// INC-KOI025: `koi approve "$id"` with an empty `$id` applied all 334 pending
/// proposals, because `"".starts_with("")` matches every id — a failed shell
/// interpolation read as `--all`. An empty or non-hex id is never a valid
/// prefix, so it is refused here rather than silently fanning out.
fn validate_id_prefix(raw: &str) -> Result<&str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!(
            "invalid proposal id: empty. An empty id is not a wildcard — pass --all \
             explicitly to act on every pending proposal."
        );
    }
    if !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!(
            "invalid proposal id {trimmed:?}: proposal ids are hex, so a prefix must be too."
        );
    }
    Ok(trimmed)
}

fn run_approve(
    all: bool,
    dry_run: bool,
    limit: Option<usize>,
    monitor: Option<String>,
    include_sensitive: bool,
    id: Option<String>,
) -> Result<()> {
    let conn = open_state()?;
    let mut pending = state::pending_proposals(&conn).context("load pending")?;
    if all {
        // Narrows before the tier split, so the held-back count reports only
        // the monitor actually being approved (TASK-KOI226).
        pending = state::filter_by_monitor(pending, monitor.as_deref())?;
    }

    // Held back from the batch path only. Naming one id still approves it —
    // that is a person reading one proposal, which is what the tier asks for.
    let mut held_back = 0usize;
    let mut to_apply: Vec<_> = if all {
        if include_sensitive {
            pending
        } else {
            let (sweepable, held) = state::partition_for_batch_approval(pending);
            held_back = held.len();
            sweepable
        }
    } else {
        let Some(raw) = id else {
            anyhow::bail!("usage: koi approve --all | koi approve <id-prefix>");
        };
        let prefix = validate_id_prefix(&raw)?;
        pending
            .into_iter()
            .filter(|p| p.id.0.starts_with(prefix))
            .collect()
    };

    if let Some(n) = limit {
        to_apply.truncate(n);
    }

    if to_apply.is_empty() {
        if held_back > 0 {
            println!(
                "No batch-approvable proposals. {held_back} held back for individual review \
                 — `koi proposals` to read them, `koi approve <id>` to take one, \
                 `koi approve --all --include-sensitive` to sweep them anyway."
            );
        } else {
            println!("No matching pending proposals.");
        }
        return Ok(());
    }

    if held_back > 0 {
        println!(
            "{held_back} content-bearing proposal(s) held back for individual review \
             (`koi approve <id>`, or --include-sensitive to sweep them)."
        );
    }

    if dry_run {
        println!("DRY RUN — {} proposal(s) would be applied:", to_apply.len());
        for p in &to_apply {
            let action: ProposedAction = serde_json::from_str(&p.action_payload)?;
            let dest_str = match &action {
                ProposedAction::Move { dest } => format!("→ {}", dest.display()),
                _ => format!("{:?}", action),
            };
            println!("  [{}] {}  {}", &p.id.0[..8], p.path.display(), dest_str);
        }
        println!("\nNo files touched. Re-run without --dry-run to apply.");
        return Ok(());
    }

    // If an approved Move's destination lands under the trash root, record
    // it in trash_log too — so proposal-driven trash moves (RootClutterMonitor's
    // tmp/backup-pattern proposals) are restorable/listable via `koi trash`,
    // not just koi dedupe apply's moves.
    let trash_root = trash::default_trash_root().ok();

    let mut applied = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for p in &to_apply {
        let action: ProposedAction =
            serde_json::from_str(&p.action_payload).context("deserialize action payload")?;
        let outcome = filing::apply(&p.path, &action);
        match outcome {
            Outcome::Applied => {
                applied += 1;
                state::record_decision(&conn, &p.id, state::Decision::Approved, Some("applied"))?;
                conn.execute(
                    "UPDATE proposals SET state = 'applied' WHERE id = ?1",
                    rusqlite::params![p.id.0],
                )?;
                if let (ProposedAction::Move { dest }, Some(root)) = (&action, &trash_root) {
                    if dest.starts_with(root) {
                        state::record_trash(&conn, &p.path, dest, chrono::Utc::now())
                            .context("record trash entry")?;
                    }
                }
                println!("  ✓ {} {}", &p.id.0[..8], p.path.display());
            }
            Outcome::Skipped(why) => {
                skipped += 1;
                // A skip is a terminal, recorded outcome — without this,
                // the exact same proposal gets re-attempted and re-skipped
                // on every future `koi approve --all` forever.
                state::record_decision(&conn, &p.id, state::Decision::Deferred, Some(&why))?;
                conn.execute(
                    "UPDATE proposals SET state = 'skipped' WHERE id = ?1",
                    rusqlite::params![p.id.0],
                )?;
                println!("  - {} {} (skipped: {why})", &p.id.0[..8], p.path.display());
            }
            Outcome::Failed(why) => {
                failed += 1;
                conn.execute(
                    "UPDATE proposals SET state = 'failed' WHERE id = ?1",
                    rusqlite::params![p.id.0],
                )?;
                println!("  ✗ {} {} (failed: {why})", &p.id.0[..8], p.path.display());
            }
        }
    }
    println!("\nApplied: {applied}, Skipped: {skipped}, Failed: {failed}");
    Ok(())
}

fn run_reject(id_prefix: Option<String>, all: bool, monitor: Option<String>) -> Result<()> {
    let conn = open_state()?;
    let pending = state::pending_proposals(&conn).context("load pending")?;

    let matches: Vec<_> = if all {
        pending
            .into_iter()
            .filter(|p| monitor.as_deref().is_none_or(|m| p.monitor == m))
            .collect()
    } else {
        let Some(raw) = id_prefix else {
            anyhow::bail!("usage: koi reject --all [--monitor <name>] | koi reject <id-prefix>");
        };
        let prefix = validate_id_prefix(&raw)?;
        pending
            .into_iter()
            .filter(|p| p.id.0.starts_with(prefix))
            .collect()
    };

    if matches.is_empty() {
        println!("No matching pending proposals.");
        return Ok(());
    }
    for p in &matches {
        state::record_decision(
            &conn,
            &ProposalId(p.id.0.clone()),
            state::Decision::Rejected,
            Some("user rejected"),
        )?;
        println!("rejected: {} {}", &p.id.0[..8], p.path.display());
    }
    println!("{} rejected.", matches.len());
    Ok(())
}

fn run_check(json: bool) -> Result<()> {
    let monitors: Vec<Box<dyn Monitor>> = vec![
        Box::new(DiskMonitor::new().context("DiskMonitor init")?),
        Box::new(BackupMonitor::new()),
        Box::new(MemoryMonitor::new()),
        Box::new(ModelSizeMonitor::new()),
        Box::new(CacheMonitor::new().context("CacheMonitor init")?),
        Box::new(DockerMonitor::new()),
        Box::new(GitMonitor::new().context("GitMonitor init")?),
        Box::new(PackageMonitor::new().context("PackageMonitor init")?),
        Box::new(NetworkMonitor::new()),
        Box::new(LatencyMonitor::new()),
        Box::new(WezTermMonitor::new()),
        Box::new(GhosttyMonitor::new()),
        // Reads the persisted snapshot only — the billing API call lives in
        // `koi cost --refresh`, because a network round trip does not fit the
        // 200ms monitor budget (TASK-KOI238).
        Box::new(koi_core::cost::CostMonitor::new()),
        // koi watching its own units (TASK-KOI232): a dead .path unit reports
        // nothing on its own, which is how one sat unnoticed for five days.
        Box::new(UnitMonitor::new()),
        // Inbox dating and aging (TASK-KOI241, ADR-0018): records first sight
        // as a side effect of looking, which is what makes aging possible.
        Box::new(koi_core::inbox_aging::InboxAgeMonitor::new().context("InboxAgeMonitor init")?),
    ];

    let reports: Vec<_> = monitors
        .iter()
        .map(|m| m.run().with_context(|| format!("{} run", m.name())))
        .collect::<Result<_>>()?;

    // Best-effort persistence — don't fail the check if DB is unreachable.
    match open_state() {
        Ok(conn) => {
            for r in &reports {
                if let Err(e) = state::record_monitor_report(&conn, r) {
                    eprintln!("warn: failed to persist {}: {e}", r.monitor);
                }
            }
        }
        Err(e) => eprintln!("warn: state unavailable, reports not persisted: {e}"),
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
        return Ok(());
    }

    for report in &reports {
        let icon = match report.status {
            HealthStatus::Healthy => "ok",
            HealthStatus::Warning => "warn",
            HealthStatus::Critical => "crit",
        };
        println!("[{icon}] {} ({} ms)", report.monitor, report.elapsed_ms);
        for obs in &report.observations {
            if let Some(gib) = obs.value.get("gib").and_then(|v| v.as_f64()) {
                println!("  - {:<10} {:>6.1} GiB", obs.key, gib);
            } else if let Some(p) = obs.value.get("pct").and_then(|v| v.as_f64()) {
                println!("  - {:<18} {:>5.1}%", obs.key, p);
            } else if obs.key == "gpu_vram_mb" {
                match obs.value.as_u64() {
                    Some(mb) => println!("  - {:<18} {:>5} MiB", obs.key, mb),
                    None => println!("  - {:<18} n/a (no NVIDIA driver)", obs.key),
                }
            }
        }
        if !report.suggestions.is_empty() {
            for s in &report.suggestions {
                println!("  • {}", s.message);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod audit_parse_tests {
    use super::*;

    #[test]
    fn count_lynis_warnings_finds_critical_lines() {
        let output = "
  [ OK ] some check
  [ WARNING ] use strong passwords
  [ CRITICAL ] outdated kernel modules found
  [ CRITICAL ] world-writable file detected
";
        assert_eq!(count_lynis_warnings(output, "critical"), 2);
        assert_eq!(count_lynis_warnings(output, "warning"), 1);
        assert_eq!(count_lynis_warnings(output, "critical"), 2);
    }

    #[test]
    fn count_lynis_warnings_zero_on_clean_output() {
        assert_eq!(count_lynis_warnings("all good", "critical"), 0);
    }

    #[test]
    fn parses_hardening_index_from_lynis_output() {
        let output = "
  ---[ Lynis 3.0.9 Results ]---

  Hardening index : [75]       [###############     ]
  Tests performed : 221
";
        assert_eq!(parse_lynis_hardening_index(output), Some(75));
    }

    #[test]
    fn hardening_index_returns_none_on_missing() {
        assert_eq!(parse_lynis_hardening_index("no score here"), None);
    }

    #[test]
    fn parses_hardening_index_from_report_dat() {
        // Key shape taken from a real Lynis 3.0.9 --report-file; the value is
        // illustrative rather than this host's actual score.
        let dat = "report_datetime_start=2026-08-20 05:46:59\n\
                   lynis_version=3.0.9\n\
                   hardening_index=42\n";
        assert_eq!(parse_report_hardening_index(dat), Some(42));
    }

    #[test]
    fn report_dat_hardening_index_none_when_absent_or_empty() {
        assert_eq!(parse_report_hardening_index("lynis_version=3.0.9\n"), None);
        // Lynis writes the key with no value when a scan is cut short.
        assert_eq!(parse_report_hardening_index("hardening_index=\n"), None);
    }

    #[test]
    fn report_warning_count_counts_only_real_warnings() {
        // Shape from a real Lynis 3.0.9 report file. Suggestions are advisory
        // and must not be counted as warnings.
        let dat = "warning[]=Some real warning|TEST-0001|\n\
                   suggestion[]=Install apt-listbugs to display critical bugs|DEB-0810|\n\
                   suggestion[]=Another suggestion|TEST-0002|\n";
        assert_eq!(parse_report_warning_count(dat), 1);
    }

    #[test]
    fn a_suggestion_mentioning_critical_is_not_a_warning() {
        // The exact line that produced a false CRITICAL desktop notification on
        // 2026-08-20: a suggestion to install apt-listbugs, matched because the
        // old counter substring-searched stdout for the word "critical".
        let dat = "suggestion[]=Install apt-listbugs to display a list of \
                   critical bugs prior to each APT installation|DEB-0810|\n";
        assert_eq!(parse_report_warning_count(dat), 0);
    }

    #[test]
    fn report_dat_parser_is_not_fooled_by_a_similar_key() {
        // A prefix match would wrongly accept this; the key must be exact.
        assert_eq!(
            parse_report_hardening_index("hardening_index_previous=99\n"),
            None
        );
    }

    #[test]
    fn parses_lynis_version() {
        let output = "[ Lynis version 3.0.9 ]";
        // Function looks for "lynis" then next token as version
        let v = parse_lynis_version(output);
        assert!(v.is_some(), "expected version to be found");
    }
}

#[cfg(test)]
mod backup_exclude_tests {
    use super::*;
    use std::ffi::OsStr;

    fn components(path: &str) -> Vec<&OsStr> {
        std::path::Path::new(path)
            .components()
            .map(|c| c.as_os_str())
            .collect()
    }

    #[test]
    fn single_component_exclude_matches_at_any_depth() {
        let excludes = vec![exclude_components("**/target/")];
        assert!(is_excluded(
            &components("myrepo/target/debug/build"),
            &excludes
        ));
        assert!(!is_excluded(
            &components("myrepo/src/target_notes.rs"),
            &excludes
        ));
    }

    #[test]
    fn multi_component_exclude_requires_consecutive_match() {
        let excludes = vec![exclude_components("**/.git/objects/")];
        assert!(is_excluded(
            &components(".git/objects/ab/cd1234"),
            &excludes
        ));
        assert!(!is_excluded(&components(".git/refs/heads/main"), &excludes));
    }

    #[test]
    fn file_level_exclude_matches_exact_name() {
        let excludes = vec![exclude_components("**/.env")];
        assert!(is_excluded(&components("myapp/.env"), &excludes));
        assert!(!is_excluded(&components("myapp/.env.example"), &excludes));
    }

    #[test]
    fn no_excludes_matches_nothing() {
        assert!(!is_excluded(&components("myrepo/src/main.rs"), &[]));
    }

    #[test]
    fn unrelated_dirs_stay_included() {
        let excludes = vec![
            exclude_components("**/target/"),
            exclude_components("**/node_modules/"),
            exclude_components("**/.cache/"),
            exclude_components("**/venv/"),
        ];
        assert!(!is_excluded(&components("myrepo/src/lib.rs"), &excludes));
        assert!(is_excluded(
            &components("web/node_modules/pkg/index.js"),
            &excludes
        ));
    }

    #[test]
    fn rclone_dest_name_uses_final_component() {
        assert_eq!(
            rclone_dest_name(std::path::Path::new("/home/x/projects")),
            "projects"
        );
        assert_eq!(
            rclone_dest_name(std::path::Path::new("/home/x/Documents/Personal")),
            "Personal"
        );
    }

    #[test]
    fn rclone_dest_names_differ_for_distinct_roots() {
        let a = rclone_dest_name(std::path::Path::new("/home/x/projects"));
        let b = rclone_dest_name(std::path::Path::new("/home/x/Documents/Personal"));
        assert_ne!(a, b);
    }

    #[test]
    fn rclone_exclude_arg_converts_directory_pattern() {
        assert_eq!(rclone_exclude_arg("**/target/"), "**/target/**");
        assert_eq!(rclone_exclude_arg("**/node_modules/"), "**/node_modules/**");
    }

    #[test]
    fn rclone_exclude_arg_passes_through_file_pattern() {
        assert_eq!(rclone_exclude_arg("**/.env"), "**/.env");
    }
}

#[cfg(test)]
mod clean_worklog_tests {
    use super::*;

    #[test]
    fn all_succeeded_summary_has_no_failure_mention() {
        let (title, changes) = clean_worklog_summary(
            5_368_709_120, // 5G
            &["huggingface cache".to_string(), "npm cache".to_string()],
            &[],
        );
        assert_eq!(title, "koi clean freed 5.0G across 2 target(s)");
        assert_eq!(
            changes,
            vec![
                "cleared huggingface cache".to_string(),
                "cleared npm cache".to_string(),
            ]
        );
    }

    #[test]
    fn failures_are_named_in_title_and_changes() {
        let (title, changes) = clean_worklog_summary(
            1024,
            &["npm cache".to_string()],
            &[("docker layer".to_string(), "permission denied".to_string())],
        );
        assert_eq!(title, "koi clean freed 1.0K across 1 target(s), 1 failed");
        assert_eq!(
            changes,
            vec![
                "cleared npm cache".to_string(),
                "failed: docker layer (permission denied)".to_string(),
            ]
        );
    }
}

#[cfg(test)]
mod git_repo_autoexclude_tests {
    use super::*;

    // Own scratch tree per test, keyed by name and pid, so parallel test
    // threads cannot collide. Matches the temp_dir convention already used in
    // koi-core rather than pulling in a new dev-dependency.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("koi-autoexclude-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nested_git_repo_is_excluded_and_loose_content_is_not() {
        let root = scratch("nested");
        let lib = root.join("amalavijnana/library");
        std::fs::create_dir_all(lib.join("nichiren-buddhism-library/.git")).unwrap();
        std::fs::create_dir_all(lib.join("books")).unwrap();

        let excludes = git_repo_excludes_under(&root, "amalavijnana/library");

        assert_eq!(
            excludes,
            vec!["amalavijnana/library/nichiren-buddhism-library/".to_string()]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn auto_discovered_repos_merge_with_static_patterns_without_duplicates() {
        let root = scratch("merge");
        let lib = root.join("amalavijnana/library");
        std::fs::create_dir_all(lib.join("nichiren-buddhism-library/.git")).unwrap();
        std::fs::create_dir_all(lib.join("books")).unwrap();

        let static_patterns = vec![
            "**/target/".to_string(),
            // The hand-maintained entry TASK-KOI166 AC-3 says must become
            // redundant. While both sources name it, the merge must not emit
            // it twice.
            "amalavijnana/library/nichiren-buddhism-library/".to_string(),
        ];

        let merged = merged_exclude_patterns(
            &static_patterns,
            std::slice::from_ref(&root),
            &["amalavijnana/library".to_string()],
        );

        assert_eq!(
            merged,
            vec![
                "**/target/".to_string(),
                "amalavijnana/library/nichiren-buddhism-library/".to_string(),
            ],
            "auto-discovery must not append a repo the static list already names"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn submodule_style_dotgit_file_still_counts_as_a_repo() {
        let root = scratch("submodule");
        let lib = root.join("amalavijnana/library");
        std::fs::create_dir_all(lib.join("linked-worktree")).unwrap();
        // A submodule or linked worktree carries .git as a FILE, not a
        // directory. Probing with is_dir() would re-include the whole tree.
        std::fs::write(lib.join("linked-worktree/.git"), "gitdir: ../../.git/x").unwrap();

        assert_eq!(
            git_repo_excludes_under(&root, "amalavijnana/library"),
            vec!["amalavijnana/library/linked-worktree/".to_string()]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn absent_tree_yields_no_excludes_rather_than_failing() {
        // The same backup.toml is meant to work across hosts, so a configured
        // tree that does not exist here is not an error.
        let root = scratch("absent");
        assert!(git_repo_excludes_under(&root, "not/here").is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn discovery_order_is_stable_regardless_of_readdir_order() {
        // readdir order is filesystem-dependent; unsorted output would make the
        // rclone argv differ between runs on identical input.
        let root = scratch("sorted");
        let lib = root.join("amalavijnana/library");
        for name in ["zulu", "alpha", "mike"] {
            std::fs::create_dir_all(lib.join(name).join(".git")).unwrap();
        }

        let found = git_repo_excludes_under(&root, "amalavijnana/library");

        assert_eq!(
            found,
            vec![
                "amalavijnana/library/alpha/".to_string(),
                "amalavijnana/library/mike/".to_string(),
                "amalavijnana/library/zulu/".to_string(),
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    // Environment-dependent, so ignored by default and run explicitly with
    // `cargo test -p koi-cli -- --ignored`. This is TASK-KOI166 AC-4: proof
    // against the operator's actual library/ tree rather than a fixture. It
    // asserts the shape (the one repo excluded, the loose folders not) rather
    // than an exact folder list, so adding reference material does not fail it.
    #[test]
    #[ignore]
    fn real_library_tree_excludes_only_its_git_repos() {
        let root = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join("projects");
        let scan_rel = "amalavijnana/library";
        if !root.join(scan_rel).is_dir() {
            eprintln!("skipping: {scan_rel} not present under {}", root.display());
            return;
        }

        let found = git_repo_excludes_under(&root, scan_rel);
        eprintln!("auto-excluded from the real tree: {found:#?}");

        assert!(
            found.contains(&"amalavijnana/library/nichiren-buddhism-library/".to_string()),
            "the one known nested repo must be excluded, got {found:?}"
        );
        for loose in [
            "books",
            "papers",
            "patents",
            "transcripts",
            "biographies",
            "philosophy",
        ] {
            let pattern = format!("{scan_rel}/{loose}/");
            assert!(
                !found.contains(&pattern),
                "loose reference material {loose} has no other backup and must stay included"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_id_is_refused_rather_than_read_as_all() {
        // INC-KOI025: this exact input applied 334 consent-gated proposals.
        for raw in ["", " ", "\t", "\n  "] {
            let err = validate_id_prefix(raw).unwrap_err().to_string();
            assert!(
                err.contains("empty"),
                "{raw:?} must be refused as empty, got: {err}"
            );
        }
    }

    #[test]
    fn non_hex_id_is_refused() {
        for raw in ["--all", "zzzz", "57090b98!", "*"] {
            assert!(
                validate_id_prefix(raw).is_err(),
                "{raw:?} is not a hex prefix and must be refused"
            );
        }
    }

    #[test]
    fn a_real_hex_prefix_is_accepted_and_trimmed() {
        assert_eq!(validate_id_prefix("57090b98").unwrap(), "57090b98");
        assert_eq!(validate_id_prefix("  e6fdd1ee  ").unwrap(), "e6fdd1ee");
        assert_eq!(
            validate_id_prefix("57090b98b6020807").unwrap(),
            "57090b98b6020807"
        );
    }

    #[test]
    fn missing_index_names_which_of_the_three_failures_it_was() {
        // TASK-KOI233 AC-4: these three shared one message until 2026-09-02,
        // so a log could not tell a koi bug from a scan that never scored.
        assert!(describe_missing_index(None).contains("run itself failed"));
        assert!(
            describe_missing_index(Some("lynis_version=3.0.9\nwarning[]=x\n"))
                .contains("no hardening_index")
        );
        assert!(describe_missing_index(Some("hardening_index=not-a-number\n")).contains("koi bug"));
    }

    #[test]
    fn a_scored_report_is_not_described_as_missing() {
        // Guard against the helper being reached on the happy path.
        assert_eq!(
            parse_report_hardening_index("hardening_index=18\n"),
            Some(18)
        );
    }

    #[test]
    fn previous_run_score_is_not_mistaken_for_this_one() {
        // Lynis writes both keys; a prefix match would return the older score.
        let dat = "hardening_index_previous=42\nhardening_index=18\n";
        assert_eq!(parse_report_hardening_index(dat), Some(18));
    }
}
