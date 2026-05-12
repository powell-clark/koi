//! Ghostty crash detection via systemd journal.
//!
//! Mirrors wezterm_crash.rs but matches "ghostty" process names. Ghostty runs
//! as a user desktop process, so the same two kernel-level signals that surface
//! for WezTerm apply here: OOM kills and segfaults in the kernel ring buffer.
//!
//! Clean exits produce no such messages, keeping the false-positive rate low.

use chrono::{Duration, Utc};

/// A detected crash event ready to be persisted.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedCrash {
    pub crash_type: String, // "oom_kill" | "segfault"
    pub pid: Option<i64>,
    pub message: String,
}

/// Query the kernel journal for Ghostty crash evidence since `since_hours` ago.
///
/// Returns `None` if `journalctl` is not available (e.g. macOS or non-systemd
/// Linux). Returns an empty `Vec` if no crashes were found.
pub fn query_ghostty_crashes(since_hours: u64) -> Option<Vec<DetectedCrash>> {
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

/// Parse kernel journal text for Ghostty crash indicators.
///
/// Returns one [`DetectedCrash`] per matching line. Pure and tested
/// independently of `journalctl`.
pub fn parse_kernel_journal(journal_text: &str) -> Vec<DetectedCrash> {
    let mut crashes = Vec::new();
    for line in journal_text.lines() {
        let lower = line.to_ascii_lowercase();
        if (lower.contains("killed process") || lower.contains("kill process"))
            && lower.contains("ghostty")
        {
            let pid = extract_pid_after_keyword(line, "process");
            crashes.push(DetectedCrash {
                crash_type: "oom_kill".into(),
                pid,
                message: line.to_string(),
            });
        } else if lower.contains("ghostty") && lower.contains("segfault") {
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

pub fn since_ts(since_hours: u64) -> chrono::DateTime<Utc> {
    Utc::now() - Duration::hours(since_hours as i64)
}

fn extract_pid_after_keyword(line: &str, keyword: &str) -> Option<i64> {
    let lower = line.to_ascii_lowercase();
    let pos = lower.find(keyword)?;
    let after = &line[pos + keyword.len()..].trim_start();
    after
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<i64>().ok())
}

fn extract_pid_from_bracket(line: &str) -> Option<i64> {
    let start = line.find('[')?;
    let end = line[start..].find(']')?;
    line[start + 1..start + end].parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_OOM: &str = "\
May 30 19:00:01 home kernel: Out of memory: Kill process 12345 (ghostty) score 512 or sacrifice child
May 30 19:00:01 home kernel: Killed process 12345 (ghostty) total-vm:1234kB, anon-rss:512kB";

    const SAMPLE_SEGFAULT: &str = "\
May 30 20:00:01 home kernel: ghostty[12345]: segfault at 0x0000000000000000 ip 00007f1234 sp 00007f5678 error 4";

    const SAMPLE_CLEAN: &str = "\
May 30 18:00:00 home kernel: e1000e 0000:00:19.0: Ring 0 is not clean
May 30 18:00:01 home kernel: usb 1-1: new full-speed USB device number 2 using xhci_hcd";

    #[test]
    fn parse_oom_kill_detected() {
        let crashes = parse_kernel_journal(SAMPLE_OOM);
        assert_eq!(crashes.len(), 2);
        assert!(crashes.iter().all(|c| c.crash_type == "oom_kill"));
        assert!(crashes.iter().all(|c| c.pid == Some(12345)));
    }

    #[test]
    fn parse_segfault_detected() {
        let crashes = parse_kernel_journal(SAMPLE_SEGFAULT);
        assert_eq!(crashes.len(), 1);
        assert_eq!(crashes[0].crash_type, "segfault");
        assert_eq!(crashes[0].pid, Some(12345));
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
    fn no_false_positive_on_wezterm_oom() {
        let line = "Killed process 49527 (wezterm-gui) total-vm:1234kB";
        let crashes = parse_kernel_journal(line);
        assert!(
            crashes.is_empty(),
            "wezterm OOM should not be detected as ghostty crash"
        );
    }

    #[test]
    fn no_false_positive_on_firefox_oom() {
        let line = "Killed process 1234 (firefox) total-vm:1234kB";
        let crashes = parse_kernel_journal(line);
        assert!(crashes.is_empty());
    }

    #[test]
    fn extract_pid_after_keyword_basic() {
        assert_eq!(
            extract_pid_after_keyword("Killed process 12345 (ghostty)", "process"),
            Some(12345)
        );
    }

    #[test]
    fn extract_pid_from_bracket_basic() {
        assert_eq!(
            extract_pid_from_bracket("ghostty[12345]: segfault at 0x0"),
            Some(12345)
        );
    }
}
