use std::fs;

use nix::{
    errno::Errno,
    sys::wait::{WaitPidFlag, WaitStatus, waitpid},
    unistd::Pid,
};
use tracing::warn;

use crate::{paths::ServedPaths, process, runner_protocol::RunnerMetadata};

/// Only identities found before this manager spawns any runners. New children
/// belong to Tokio Child wait tasks; never use waitpid(-1) here.
pub(super) struct InheritedRunners(Vec<(Pid, u64)>);

impl InheritedRunners {
    pub(super) fn capture(paths: &ServedPaths) -> Self {
        let mut children = Vec::new();
        if let Ok(entries) = fs::read_dir(paths.runners_dir()) {
            for entry in entries.flatten() {
                let metadata = fs::read(entry.path().join("runner.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<RunnerMetadata>(&bytes).ok());
                let Some(metadata) = metadata else { continue };
                if entry.file_name().to_str() != Some(&metadata.name) {
                    continue;
                }
                let Ok(pid) = i32::try_from(metadata.runner_pid) else {
                    continue;
                };
                if pid <= 0 {
                    continue;
                }
                let Some(started) = process::start_time(metadata.runner_pid) else {
                    continue;
                };
                if metadata
                    .runner_start_time
                    .is_some_and(|expected| expected != started)
                {
                    continue;
                }
                children.push((Pid::from_raw(pid), started));
            }
        }
        Self(children)
    }

    pub(super) fn reap(&mut self) {
        self.0.retain(|&(pid, started)| {
            if process::start_time(pid.as_raw() as u32) != Some(started) {
                return false;
            }
            match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) | Err(Errno::EINTR) => true,
                // Crash/relinquish adoption can find runners parented elsewhere.
                Err(Errno::ECHILD) | Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => false,
                Ok(_) => true,
                Err(error) => {
                    warn!(%pid, %error, "cannot reap inherited runner");
                    true
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::Command,
        time::{Duration, Instant},
    };

    fn record(paths: &ServedPaths, name: &str, pid: u32, started: u64) {
        fs::create_dir_all(paths.runner_dir(name)).unwrap();
        fs::write(
            paths.runner_metadata(name),
            serde_json::to_vec(&RunnerMetadata {
                name: name.to_owned(),
                runner_pid: pid,
                runner_start_time: Some(started),
                service_pid: None,
                service_start_time: None,
            })
            .unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn reaps_only_captured_children_and_preserves_tokio_exit_status() {
        let root = tempfile::tempdir().unwrap();
        let paths = ServedPaths::from_home(root.path());
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        record(&paths, "api", pid, process::start_time(pid).unwrap());
        let mut inherited = InheritedRunners::capture(&paths);
        inherited.reap();
        assert_eq!(inherited.0.len(), 1);
        // A child started later has a separate Tokio owner.
        let mut later = tokio::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        child.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !inherited.0.is_empty() && Instant::now() < deadline {
            inherited.reap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(inherited.0.is_empty());
        assert_eq!(
            child.wait().unwrap_err().raw_os_error(),
            Some(Errno::ECHILD as i32)
        );
        assert_eq!(later.wait().await.unwrap().code(), Some(7));
    }

    #[test]
    fn discards_nonchildren_and_mismatched_identities() {
        let root = tempfile::tempdir().unwrap();
        let paths = ServedPaths::from_home(root.path());
        let pid = std::process::id();
        let started = process::start_time(pid).unwrap();
        record(&paths, "self", pid, started);
        record(&paths, "stale", pid, started + 1);
        let mut inherited = InheritedRunners::capture(&paths);
        assert_eq!(inherited.0.len(), 1);
        inherited.reap();
        assert!(inherited.0.is_empty());
    }
}
