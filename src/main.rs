use std::{path::Path, process};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use served::{
    client,
    config::{CONFIG_FILE, write_template},
    editor, manager,
    paths::ServedPaths,
    protocol::{Request, Response, Target},
    runner, tui,
};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "served",
    version,
    about = "lightweight service manager for one installation user"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the manager. Normally started by the served system service.
    Daemon {
        #[arg(long, hide = true)]
        handoff: bool,
    },
    /// Stop the manager and all runners. Used by the system service.
    #[command(hide = true)]
    Shutdown,
    /// Replace the manager process while keeping runners alive.
    #[command(hide = true)]
    Runner {
        #[arg(long)]
        name: String,
        #[arg(long)]
        socket: std::path::PathBuf,
    },
    /// Open .served.json in an external editor.
    Edit {
        #[arg(short = 'e', long, value_name = "COMMAND", conflicts_with = "path")]
        editor: Option<String>,
        #[arg(long, conflicts_with = "editor")]
        path: bool,
    },
    /// Enable the current service directory and start it.
    Enable,
    /// Disable the current service, or an enabled service by name.
    Disable { name: Option<String> },
    /// Restart the current service, or an enabled service by name.
    Restart { name: Option<String> },
    /// Attach directly to the current service, or an enabled service by name.
    Attach { name: Option<String> },
    /// Open service output history in an editor, or print its path.
    History {
        name: Option<String>,
        #[arg(long)]
        run: Option<String>,
        #[arg(short = 'e', long, value_name = "COMMAND", conflicts_with = "path")]
        editor: Option<String>,
        #[arg(long, conflicts_with = "editor")]
        path: bool,
    },
    /// Print enabled services known to the manager.
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .try_init()
        .ok();

    let cli = Cli::parse();
    match cli.command {
        Some(Command::Edit { editor, path }) => {
            let directory = std::env::current_dir().context("read current directory")?;
            edit_config(&directory, editor, path).await
        }
        command => {
            let paths = ServedPaths::from_environment().context("served requires HOME")?;
            match command {
                None => tui::run(paths).await,
                Some(Command::Daemon { handoff }) => {
                    if handoff {
                        manager::request_handoff(paths).await
                    } else {
                        manager::run_daemon(paths).await
                    }
                }
                Some(Command::Shutdown) => manager::request_shutdown(paths).await,
                Some(Command::Runner { name, socket }) => runner::run(name, socket).await,
                Some(Command::Enable) => {
                    let directory = std::env::current_dir().context("read current directory")?;
                    client::expect_ok(
                        &paths,
                        Request::Enable {
                            directory: directory.display().to_string(),
                        },
                    )
                    .await
                }
                Some(Command::Disable { name }) => {
                    let target = client::target(name, std::env::current_dir()?);
                    client::expect_ok(&paths, Request::Disable { target }).await
                }
                Some(Command::Restart { name }) => {
                    let target = client::target(name, std::env::current_dir()?);
                    client::expect_ok(&paths, Request::Restart { target }).await
                }
                Some(Command::Attach { name }) => tui::attach(paths, name).await,
                Some(Command::History {
                    name,
                    run,
                    editor,
                    path,
                }) => {
                    let target = client::target(name, std::env::current_dir()?);
                    print_history(&paths, target, run, editor, path).await
                }
                Some(Command::List) => print_list(&paths).await,
                Some(Command::Edit { .. }) => unreachable!("edit is handled above"),
            }
        }
    }
}

async fn print_list(paths: &ServedPaths) -> Result<()> {
    let response = client::request(paths, Request::List).await?;
    let Response::Services { services } = response else {
        bail!("unexpected manager response")
    };
    for service in services {
        println!(
            "{:<18} {:<11} pid={:<7} tty={} restart={} {}",
            service.name,
            format_state(&service.state),
            service
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            service.tty,
            service.restart,
            service.directory
        );
    }
    Ok(())
}

async fn print_history(
    paths: &ServedPaths,
    target: Target,
    run: Option<String>,
    editor: Option<String>,
    path_only: bool,
) -> Result<()> {
    let response = client::request(paths, Request::HistoryList { target }).await?;
    let Response::HistoryList { service, records } = response else {
        bail!("unexpected manager response")
    };
    let id = run.unwrap_or_else(|| "latest".to_owned());
    let record = records
        .iter()
        .find(|record| record.id == id)
        .ok_or_else(|| anyhow::anyhow!("history record {id:?} was not found"))?;
    if !record.persisted {
        bail!(
            "history record {id:?} is memory-only and has no path; enable persist_logs or use the TUI history browser"
        );
    }

    let file_name = if id == "latest" {
        "latest.log"
    } else {
        id.as_str()
    };
    let path = paths.logs_dir().join(service).join(file_name);
    if path_only {
        println!("{}", path.display());
        return Ok(());
    }

    let editor = editor::resolve(editor)?;
    open_editor_or_exit(&editor, &path).await
}

async fn edit_config(directory: &Path, editor: Option<String>, path_only: bool) -> Result<()> {
    write_template(directory).context("create .served.json template")?;
    let path = directory.join(CONFIG_FILE);
    if path_only {
        println!("{}", path.display());
        return Ok(());
    }

    let editor = editor::resolve(editor)?;
    open_editor_or_exit(&editor, &path).await
}

async fn open_editor_or_exit(editor_command: &str, path: &Path) -> Result<()> {
    let status = editor::run(editor_command, path).await?;
    if status.success() {
        return Ok(());
    }
    if let Some(code) = status.code() {
        process::exit(code);
    }
    bail!("external editor terminated by signal")
}

fn format_state(state: &served::protocol::ServiceState) -> &'static str {
    match state {
        served::protocol::ServiceState::Starting => "starting",
        served::protocol::ServiceState::Running => "running",
        served::protocol::ServiceState::Restarting => "restarting",
        served::protocol::ServiceState::Stopped => "stopped",
        served::protocol::ServiceState::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn attach_accepts_an_optional_name() {
        let without_name = Cli::try_parse_from(["served", "attach"]).expect("parse attach");
        assert!(matches!(
            without_name.command,
            Some(Command::Attach { name: None })
        ));

        let with_name =
            Cli::try_parse_from(["served", "attach", "api"]).expect("parse named attach");
        assert!(matches!(
            with_name.command,
            Some(Command::Attach { name: Some(name) }) if name == "api"
        ));
    }

    #[test]
    fn edit_accepts_editor_and_path_options() {
        let command =
            Cli::try_parse_from(["served", "edit", "-e", "nvim -f"]).expect("parse edit editor");
        assert!(matches!(
            command.command,
            Some(Command::Edit {
                editor: Some(editor),
                path: false,
            }) if editor == "nvim -f"
        ));

        let path = Cli::try_parse_from(["served", "edit", "--path"]).expect("parse edit path");
        assert!(matches!(
            path.command,
            Some(Command::Edit {
                editor: None,
                path: true,
            })
        ));
        assert!(Cli::try_parse_from(["served", "edit", "--path", "-e", "nvim"]).is_err());
    }

    #[tokio::test]
    async fn edit_path_creates_template_without_editor() {
        let directory = tempdir().expect("tempdir");

        edit_config(directory.path(), None, true)
            .await
            .expect("create config path");

        let path = directory.path().join(CONFIG_FILE);
        assert!(path.is_file());
        assert!(
            std::fs::read_to_string(path)
                .expect("read config")
                .contains("// Globally unique service name")
        );
    }

    #[test]
    fn history_accepts_optional_name_and_run_selector() {
        let command = Cli::try_parse_from([
            "served",
            "history",
            "api",
            "--run",
            "20260724-233045.log",
            "-e",
            "nvim -f",
        ])
        .expect("parse history");
        assert!(matches!(
            command.command,
            Some(Command::History {
                name: Some(name),
                run: Some(run),
                editor: Some(editor),
                path: false,
            }) if name == "api" && run == "20260724-233045.log" && editor == "nvim -f"
        ));

        let path = Cli::try_parse_from(["served", "history", "api", "--path"])
            .expect("parse history path");
        assert!(matches!(
            path.command,
            Some(Command::History {
                path: true,
                editor: None,
                ..
            })
        ));
        assert!(Cli::try_parse_from(["served", "history", "api", "--path", "-e", "nvim"]).is_err());
    }
}
