use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use koi_core::{
    cleaners,
    filing::{
        self, DocumentsMonitor, DownloadsMonitor, FileMonitor, GoogleDriveMonitor, InboxMonitor,
        Outcome, ProposalId, ProposedAction, ScanContext, SqliteClassifier,
    },
    monitors::{
        CacheMonitor, DiskMonitor, DockerMonitor, GhosttyMonitor, GitMonitor, LatencyMonitor,
        MemoryMonitor, NetworkMonitor, PackageMonitor, WezTermMonitor,
    },
    notes, state,
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
    },
    /// Start continuous health monitoring.
    Monitor,
    /// Scan accumulation points and emit filing proposals (no mutations).
    Scan {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// List pending filing proposals.
    Proposals,
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
        /// Proposal id (hex prefix).
        id: Option<String>,
    },
    /// Reject a pending proposal (records signal, does nothing on disk).
    Reject { id: String },
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
        } => run_backup(dry_run, include_red)?,
        Command::Monitor => run_monitor()?,
        Command::Scan { json } => run_scan(json)?,
        Command::Proposals => run_proposals()?,
        Command::Approve {
            all,
            dry_run,
            limit,
            id,
        } => run_approve(all, dry_run, limit, id)?,
        Command::Reject { id } => run_reject(&id)?,
        Command::History { monitor, limit } => run_history(&monitor, limit)?,
        Command::Stats => run_stats()?,
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "koi", &mut std::io::stdout());
        }
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
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("$HOME not set"))?;
    let audits_dir = home.join(".local/share/koi/audits");
    std::fs::create_dir_all(&audits_dir).context("create audits directory")?;

    let ts = chrono::Utc::now();
    let ts_str = ts.format("%Y%m%d-%H%M%S").to_string();
    let report_path = audits_dir.join(format!("lynis-{ts_str}.log"));

    println!("Running Lynis{}…", if quick { " (quick)" } else { "" });

    let mut cmd = std::process::Command::new("lynis");
    cmd.arg("audit")
        .arg("system")
        .arg("--no-colors")
        .arg("--quiet");
    if quick {
        cmd.args([
            "--tests-from-group",
            "authentication,filesystems,networking",
        ]);
    }

    let output = cmd.output().context("spawn lynis")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, String::from_utf8_lossy(&output.stderr));

    // Write raw log to file.
    std::fs::write(&report_path, combined.as_bytes()).context("write audit log")?;

    // Parse hardening index from the Lynis output.
    let hardening_index = parse_lynis_hardening_index(&stdout);
    let lynis_version = parse_lynis_version(&stdout);

    if let Some(score) = hardening_index {
        println!("Hardening index: {score}/100");
    } else {
        println!("Hardening index: (not found in output)");
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

    // Desktop notification when score drops or critical warnings found.
    let critical_count = count_lynis_warnings(&stdout, "critical");
    audit_notify(hardening_index, prev_score, critical_count, &report_path);

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
            "{score_msg} | {critical_count} critical warning(s)
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

/// Extract the Lynis hardening index from stdout.
/// Looks for: "  Hardening index : [<n>]"
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

    let home: PathBuf = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("$HOME not set"))?;

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
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("$HOME not set"))?;
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

fn run_zones(custom_roots: Vec<std::path::PathBuf>) -> Result<()> {
    use koi_core::filing::managed_zone;

    let roots = if !custom_roots.is_empty() {
        custom_roots
    } else {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("$HOME not set"))?;
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

fn run_clean(dry_run: bool) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("$HOME not set"))?;
    let plan = cleaners::plan(&home);

    let mut total_bytes: u64 = 0;
    let mut to_clean: Vec<_> = plan
        .into_iter()
        .filter(|(_, _, existed, _)| *existed)
        .collect();
    to_clean.sort_by(|a, b| b.3.cmp(&a.3));

    if to_clean.is_empty() {
        println!("Nothing to clean — all safe cache targets are already absent.");
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
        return Ok(());
    }

    let mut freed = 0u64;
    let mut failed = Vec::<(String, String)>::new();
    for (target, path, _, size) in &to_clean {
        match cleaners::execute_target(path) {
            Ok(()) => {
                freed += size;
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
    Ok(())
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

fn run_backup(dry_run: bool, include_red: bool) -> Result<()> {
    use std::path::PathBuf;

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("$HOME not set"))?;

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
    fn collect_tier(roots: &[PathBuf], to_upload: &mut Vec<(PathBuf, u64)>) -> (usize, u64) {
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

    let amber_roots = read_roots("amber_tier", "include");
    let red_roots = if include_red {
        read_roots("red_tier", "paths")
    } else {
        Vec::new()
    };

    // Collect files to upload, tracking each tier's count as we go.
    let mut to_upload: Vec<(PathBuf, u64)> = Vec::new();
    let (amber_count, amber_bytes) = collect_tier(&amber_roots, &mut to_upload);
    let (red_count, red_bytes) = collect_tier(&red_roots, &mut to_upload);
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

    if dry_run {
        println!("\nDry run — no files uploaded.");
        println!("Run without --dry-run to sync to rclone remote 'koi-crypt'.");
        return Ok(());
    }

    // Execute rclone sync. The source root is config/env-driven: KOI_BACKUP_SOURCE
    // wins, otherwise the first configured amber-tier include is used. No personal
    // directory layout is assumed here.
    let sync_source = std::env::var_os("KOI_BACKUP_SOURCE")
        .map(PathBuf::from)
        .or_else(|| amber_roots.first().cloned());
    let Some(sync_source) = sync_source else {
        anyhow::bail!(
            "No backup source: set KOI_BACKUP_SOURCE or add an amber_tier.include entry to backup.toml"
        );
    };
    println!("\nStarting rclone sync to koi-crypt:/...");
    let command = std::process::Command::new("rclone")
        .args([
            "sync",
            "--verbose",
            "--progress",
            sync_source.to_str().unwrap_or("."),
            "koi-crypt:/",
        ])
        .output();

    match command {
        Ok(output) => {
            if output.status.success() {
                println!("✓ Backup completed successfully.");
                println!("\nSynced: {}", human(total_bytes));
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("rclone sync failed:\n{}", stderr)
            }
        }
        Err(e) => {
            anyhow::bail!("Failed to run rclone: {}", e)
        }
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
        "MemoryMonitor",
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

fn run_scan(json: bool) -> Result<()> {
    let mut monitors: Vec<Box<dyn FileMonitor>> = vec![
        Box::new(DownloadsMonitor::new().context("DownloadsMonitor init")?),
        Box::new(DocumentsMonitor::new().context("DocumentsMonitor init")?),
        Box::new(InboxMonitor::new().context("InboxMonitor init")?),
    ];
    if let Some(gdrive) = GoogleDriveMonitor::load() {
        monitors.push(Box::new(gdrive));
    }

    // Build scan context with a classifier reading from the DB (learning loop).
    let classifier_conn = open_state()?;
    let ctx =
        ScanContext::new_now().with_classifier(Box::new(SqliteClassifier::new(classifier_conn)));

    let mut all_proposals = Vec::new();
    for m in &monitors {
        let proposals = m.scan(&ctx).with_context(|| format!("{} scan", m.name()))?;
        all_proposals.extend(proposals);
    }

    // Persist proposals — idempotent upsert, safe to re-scan. Separate
    // write-connection from the classifier's read-connection.
    let conn = open_state()?;
    for p in &all_proposals {
        state::upsert_proposal(&conn, p).context("persist proposal")?;
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&all_proposals)?);
        return Ok(());
    }

    if all_proposals.is_empty() {
        println!(
            "No proposals — accumulation points are clean or contain only unknown-type files."
        );
        return Ok(());
    }

    println!(
        "{} proposal(s) — stored; review with `koi proposals`, apply with `koi approve --all`:",
        all_proposals.len()
    );
    for p in &all_proposals {
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

fn run_proposals() -> Result<()> {
    let conn = open_state()?;
    let pending = state::pending_proposals(&conn).context("load pending")?;
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

fn run_approve(all: bool, dry_run: bool, limit: Option<usize>, id: Option<String>) -> Result<()> {
    let conn = open_state()?;
    let pending = state::pending_proposals(&conn).context("load pending")?;

    let mut to_apply: Vec<_> = if all {
        pending
    } else {
        let Some(prefix) = id else {
            anyhow::bail!("usage: koi approve --all | koi approve <id-prefix>");
        };
        pending
            .into_iter()
            .filter(|p| p.id.0.starts_with(&prefix))
            .collect()
    };

    if let Some(n) = limit {
        to_apply.truncate(n);
    }

    if to_apply.is_empty() {
        println!("No matching pending proposals.");
        return Ok(());
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
                println!("  ✓ {} {}", &p.id.0[..8], p.path.display());
            }
            Outcome::Skipped(why) => {
                skipped += 1;
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

fn run_reject(id_prefix: &str) -> Result<()> {
    let conn = open_state()?;
    let pending = state::pending_proposals(&conn).context("load pending")?;
    let matches: Vec<_> = pending
        .into_iter()
        .filter(|p| p.id.0.starts_with(id_prefix))
        .collect();
    if matches.is_empty() {
        anyhow::bail!("no pending proposal matches id prefix {id_prefix}");
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
    Ok(())
}

fn run_check(json: bool) -> Result<()> {
    let monitors: Vec<Box<dyn Monitor>> = vec![
        Box::new(DiskMonitor::new().context("DiskMonitor init")?),
        Box::new(MemoryMonitor::new()),
        Box::new(CacheMonitor::new().context("CacheMonitor init")?),
        Box::new(DockerMonitor::new()),
        Box::new(GitMonitor::new().context("GitMonitor init")?),
        Box::new(PackageMonitor::new().context("PackageMonitor init")?),
        Box::new(NetworkMonitor::new()),
        Box::new(LatencyMonitor::new()),
        Box::new(WezTermMonitor::new()),
        Box::new(GhosttyMonitor::new()),
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
    fn parses_lynis_version() {
        let output = "[ Lynis version 3.0.9 ]";
        // Function looks for "lynis" then next token as version
        let v = parse_lynis_version(output);
        assert!(v.is_some(), "expected version to be found");
    }
}
