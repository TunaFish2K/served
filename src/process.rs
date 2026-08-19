use std::{fs, path::Path};

pub(crate) fn matches(pid: u32, expected_start_time: Option<u64>) -> bool {
    if !Path::new(&format!("/proc/{pid}")).exists() {
        return false;
    }
    expected_start_time.is_none_or(|expected| start_time(pid) == Some(expected))
}

pub(crate) fn start_time(pid: u32) -> Option<u64> {
    let content = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = content.rsplit_once(") ")?;
    fields.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_matches_its_start_time() {
        let pid = std::process::id();
        let started = start_time(pid).expect("current process start time");
        assert!(matches(pid, Some(started)));
        assert!(!matches(pid, Some(started.saturating_add(1))));
    }

    #[test]
    fn missing_process_does_not_match() {
        assert!(!matches(u32::MAX, None));
    }
}
