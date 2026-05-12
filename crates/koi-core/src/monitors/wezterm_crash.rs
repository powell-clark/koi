//! WezTerm crash detection via systemd journal.
//!
//! WezTerm runs as a user desktop process (not a systemd service), so there is
//! no unit exit code to inspect. Instead, two kernel-level signals surface in
//! the journal and are reliable crash indicators:
//!
//! 1. **OOM kill** — the kernel or systemd-oomd emits "Killed process <pid>
//!    (wezterm-gui)" in the kernel ring buffer when memory pressure triggers a
//!    kill. This is distinct from a clean exit because the message appears *in
//!    the kernel journal* (priority 3, syslog identifier = "kernel").
//!
//! 2. **Segfault** — the kernel emits "wezterm-gui[<pid>]: segfault at …" into
//!    the kernel ring buffer on a SIGSEGV/SIGBUS fault.
//!
//! Clean exits (normal window-close, logout, reboot) produce neither of these
//! kernel messages, so false-positive rate is low.
//!
//! The implementation shells to `journalctl` with `-k` (kernel messages only)
//! so parsing is fast and the result set is small.

use chrono::{Duration, Utc};

/// A detected crash event ready to be persisted.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedCrash {
    pub crash_type: String, // "oom_kill" | "segfault"
    pub pid: Option<i64>,
    pub message: String,
}

/// Query the kernel journal for WezTerm crash evidence since `since_hours` ago.
///
/// Returns `None` if `journalctl` is not available (e.g. macOS or non-systemd
/// Linux). Returns an empty `Vec` if no crashes were found.
pub fn query_wezterm_crashes(since_hours: u64) -> Option<Vec<DetectedCrash>> {
    let since = format!("{} hours ago", since_hours);
    let out = std::process::Command::new("journalctl")
        .args(["-k", "--no-pager", "-n", "500", "--since", &since])
        .output()
        .ok()?;

    if !out.status.success() && out.stdout.is_empty() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    Some(parse_kernel_journal(&text))
}

/// Parse kernel journal text for WezTerm crash indicators.
///
/// Returns one [`DetectedCrash`] per matching line. The function is pure and
/// tested independently of `journalctl`.
pub fn parse_kernel_journal(journal_text: &str) -> Vec<DetectedCrash> {
    let mut crashes = Vec::new();
    for line in journal_text.lines() {
        let lower = line.to_ascii_lowercase();
        // OOM kill: "Killed process <pid> (wezterm" or "Out of memory: Kill process <pid> (wezterm"
        if (lower.contains("killed process") || lower.contains("kill process"))
            && lower.contains("wezterm")
        {
            let pid = extract_pid_after_keyword(line, "process");
            crashes.push(DetectedCrash {
                crash_type: "oom_kill".into(),
                pid,
                message: line.to_string(),
            });
        } else if lower.contains("wezterm") && lower.contains("segfault") {
            let pid = extract_pid_from_bracket(line);
            crashes.push(DetectedCrash {
                crash_type: "segfault".into(),
                pid,
                message: line.to_string(),
            });
        }
    }
    crashes
}

/// Derive an RFC-3339 "since" string to avoid re-scanning already-recorded
/// crashes. Returns a window of `since_hours` hours from now.
pub fn since_ts(since_hours: u64) -> chrono::DateTime<Utc> {
    Utc::now() - Duration::hours(since_hours as i64)
}

// -- PID extraction helpers --------------------------------------------------

/// Extract the first integer after the word `keyword` (case-insensitive).
/// e.g. "Killed process 49527 (wezterm-gui)" with keyword "process" → Some(49527)
fn extract_pid_after_keyword(line: &str, keyword: &str) -> Option<i64> {
    let lower = line.to_ascii_lowercase();
    let pos = lower.find(keyword)?;
    let after = &line[pos + keyword.len()..].trim_start();
    after
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<i64>().ok())
}

/// Extract PID from a "[pid]:" bracket pattern in a line.
/// e.g. "wezterm-gui[49527]: segfault at 0x0" → Some(49527)
fn extract_pid_from_bracket(line: &str) -> Option<i64> {
    let start = line.find('[')?;
    let end = line[start..].find(']')?;
    line[start + 1..start + end].parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_OOM: &str = "\
May 30 19:00:01 home kernel: Out of memory: Kill process 49527 (wezterm-gui) score 512 or sacrifice child
May 30 19:00:01 home kernel: Killed process 49527 (wezterm-gui) total-vm:1234kB, anon-rss:512kB";

    const SAMPLE_SEGFAULT: &str = "\
May 30 20:00:01 home kernel: wezterm-gui[49527]: segfault at 0x0000000000000000 ip 00007f1234 sp 00007f5678 error 4 in wezterm-gui[7f000+200000]";

    const SAMPLE_CLEAN: &str = "\
May 30 18:00:00 home kernel: e1000e 0000:00:19.0: Ring 0 is not clean
May 30 18:00:01 home kernel: usb 1-1: new full-speed USB device number 2 using xhci_hcd";

    #[test]
    fn parse_oom_kill_detected() {
        let crashes = parse_kernel_journal(SAMPLE_OOM);
        // Two lines match (one "Kill process", one "Killed process")
        assert_eq!(crashes.len(), 2);
        assert!(crashes.iter().all(|c| c.crash_type == "oom_kill"));
        assert!(crashes.iter().all(|c| c.pid == Some(49527)));
    }

    #[test]
    fn parse_segfault_detected() {
        let crashes = parse_kernel_journal(SAMPLE_SEGFAULT);
        assert_eq!(crashes.len(), 1);
        assert_eq!(crashes[0].crash_type, "segfault");
        assert_eq!(crashes[0].pid, Some(49527));
    }

    #[test]
    fn parse_clean_journal_no_crashes() {
        let crashes = parse_kernel_journal(SAMPLE_CLEAN);
        assert!(crashes.is_empty());
    }

    #[test]
    fn parse_empty_journal_no_crashes() {
        assert!(parse_kernel_journal("").is_empty());
    }

    #[test]
    fn extract_pid_after_keyword_basic() {
        assert_eq!(
            extract_pid_after_keyword("Killed process 49527 (wezterm-gui)", "process"),
            Some(49527)
        );
    }

    #[test]
    fn extract_pid_from_bracket_basic() {
        assert_eq!(
            extract_pid_from_bracket("wezterm-gui[49527]: segfault at 0x0"),
            Some(49527)
        );
    }

    #[test]
    fn extract_pid_from_bracket_no_bracket() {
        assert_eq!(extract_pid_from_bracket("no brackets here"), None);
    }

    #[test]
    fn no_false_positive_on_non_wezterm_oom() {
        let line = "Killed process 1234 (firefox) total-vm:1234kB";
        let crashes = parse_kernel_journal(line);
        assert!(
            crashes.is_empty(),
            "firefox OOM should not be detected as wezterm crash"
        );
    }
}
