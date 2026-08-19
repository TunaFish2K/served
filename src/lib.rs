#![forbid(unsafe_code)]

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
