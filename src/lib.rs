#![forbid(unsafe_code)]

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("served supports Linux and macOS only");

pub mod cli;
pub mod client;
pub mod config;
pub mod editor;
mod ipc;
pub mod logs;
pub mod manager;
pub mod paths;
mod process;
pub mod protocol;
pub mod runner;
pub mod runner_protocol;
pub mod tui;
pub mod worker;
