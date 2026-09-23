#[cfg(target_os = "linux")]
use std::fs;

#[cfg(target_os = "macos")]
use sysinfo::{ProcessStatus, ProcessesToUpdate, System};

struct ProcessInfo {
    start_time: u64,
    alive: bool,
}

pub(crate) fn matches(pid: u32, expected_start_time: Option<u64>) -> bool {
    inspect(pid).is_some_and(|info| {
        info.alive && expected_start_time.is_none_or(|expected| info.start_time == expected)
    })
}

// Some platforms retain zombie identity; macOS may no longer expose it after exit.
pub(crate) fn start_time(pid: u32) -> Option<u64> {
    inspect(pid).map(|info| info.start_time)
}

#[cfg(target_os = "linux")]
fn inspect(pid: u32) -> Option<ProcessInfo> {
    let content = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&content)
}

#[cfg(target_os = "linux")]
fn parse_stat(content: &str) -> Option<ProcessInfo> {
    let (_, fields) = content.rsplit_once(") ")?;
    let mut fields = fields.split_whitespace();
    let state = fields.next()?;
    let start_time = fields.nth(18)?.parse().ok()?;
    Some(ProcessInfo {
        start_time,
        alive: !matches!(state, "Z" | "X" | "x"),
    })
}

#[cfg(target_os = "macos")]
fn inspect(pid: u32) -> Option<ProcessInfo> {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).map(|process| ProcessInfo {
        start_time: process.start_time(),
        // sysinfo's Dead status on macOS means an uninterruptible thread, not exit.
        alive: process.status() != ProcessStatus::Zombie,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_matches_its_start_time() {
        let pid = std::process::id();
        let started = start_time(pid).expect("current process start time");
        assert!(matches(pid, Some(started)));
        assert!(matches(pid, None));
        assert!(!matches(pid, Some(started.saturating_add(1))));
    }

    #[test]
    fn missing_process_does_not_match() {
        assert!(!matches(u32::MAX, None));
        assert!(!matches(u32::MAX, Some(1)));
    }

    #[test]
    fn zombie_is_not_alive_even_when_identity_is_unavailable() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();
        let started = start_time(pid).unwrap();
        child.kill().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while matches(pid, None) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let still_alive = matches(pid, None);
        let still_matches = matches(pid, Some(started));
        let zombie_identity = start_time(pid);
        child.wait().unwrap();
        assert!(!still_alive);
        assert!(!still_matches);
        #[cfg(target_os = "linux")]
        assert_eq!(zombie_identity, Some(started));
        #[cfg(target_os = "macos")]
        assert!(zombie_identity.is_none_or(|identity| identity == started));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stat_parser_distinguishes_exit_from_stopped_or_uninterruptible_processes() {
        for state in ["R", "S", "T", "t", "D", "Z", "X", "x"] {
            let line = format!(
                "123 (name with ) parentheses) {state} {} 456",
                "0 ".repeat(18)
            );
            let info = parse_stat(&line).unwrap();
            assert_eq!(info.start_time, 456);
            assert_eq!(info.alive, !matches!(state, "Z" | "X" | "x"));
        }
        assert!(parse_stat("invalid").is_none());
    }
}
