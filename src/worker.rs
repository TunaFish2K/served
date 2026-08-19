use std::{collections::BTreeMap, time::Duration};

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};

use crate::{
    config::LoadedService,
    protocol::{HandoffStream, Target},
};

mod runtime;

const BACKOFF_BASE: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
pub const WORKER_EVENT_CAPACITY: usize = 256;

#[derive(Debug)]
pub enum WorkerCommand {
    Stop {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Restart {
        service: LoadedService,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Attach {
        stream: HandoffStream,
    },
    Resize {
        cols: u16,
        rows: u16,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Debug)]
pub enum WorkerEvent {
    Starting {
        name: String,
        persist_logs: bool,
    },
    Started {
        name: String,
        pid: u32,
        tty: bool,
    },
    Output {
        name: String,
        bytes: Vec<u8>,
    },
    Exited {
        name: String,
        code: Option<i32>,
        success: bool,
    },
    Restarting {
        name: String,
        delay: Duration,
    },
    Stopped {
        name: String,
    },
    Failed {
        name: String,
        error: String,
    },
    AttachChanged {
        name: String,
        active: bool,
    },
}

pub fn spawn_service(
    service: LoadedService,
    manager_environment: BTreeMap<String, String>,
    events: mpsc::Sender<WorkerEvent>,
) -> mpsc::Sender<WorkerCommand> {
    let (commands, receiver) = mpsc::channel(16);
    tokio::spawn(runtime::run_service(
        service,
        manager_environment,
        events,
        receiver,
    ));
    commands
}

pub fn backoff(attempt: u32) -> Duration {
    let multiplier = 1_u64 << attempt.saturating_sub(1).min(7);
    let millis = BACKOFF_BASE
        .as_millis()
        .saturating_mul(u128::from(multiplier))
        .min(BACKOFF_MAX.as_millis());
    Duration::from_millis(millis as u64)
}

pub fn target_name(target: &Target) -> Option<&str> {
    match target {
        Target::Name(name) => Some(name),
        Target::Directory(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff(1), Duration::from_millis(250));
        assert_eq!(backoff(2), Duration::from_millis(500));
        assert_eq!(backoff(8), Duration::from_secs(30));
        assert_eq!(backoff(100), Duration::from_secs(30));
    }
}
