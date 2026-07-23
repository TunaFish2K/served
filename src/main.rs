use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use served::{
    client, manager,
    paths::ServedPaths,
    protocol::{Request, Response},
    tui,
};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "served",
    version,
    about = "lightweight per-user service manager"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the per-user manager. Normally started by systemd --user.
    Daemon,
    /// Edit .served.json and .env in the current directory.
    Edit,
    /// Enable the current service directory and start it.
    Enable,
    /// Disable the current service, or an enabled service by name.
    Disable { name: Option<String> },
    /// Restart the current service, or an enabled service by name.
    Restart { name: Option<String> },
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
        Some(Command::Edit) => {
            tui::edit_current(&std::env::current_dir().context("read current directory")?)
        }
        command => {
            let paths =
                ServedPaths::from_environment().context("served requires XDG_RUNTIME_DIR")?;
            match command {
                None => tui::run(paths).await,
                Some(Command::Daemon) => manager::run_daemon(paths).await,
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
                Some(Command::List) => print_list(&paths).await,
                Some(Command::Edit) => unreachable!("edit is handled above"),
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

fn format_state(state: &served::protocol::ServiceState) -> &'static str {
    match state {
        served::protocol::ServiceState::Starting => "starting",
        served::protocol::ServiceState::Running => "running",
        served::protocol::ServiceState::Restarting => "restarting",
        served::protocol::ServiceState::Stopped => "stopped",
        served::protocol::ServiceState::Failed => "failed",
    }
}
