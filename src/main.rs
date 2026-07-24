use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use served::{
    client, manager,
    paths::ServedPaths,
    protocol::{Request, Response, Target},
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
    /// Attach directly to the current service, or an enabled service by name.
    Attach { name: Option<String> },
    /// List or print service output history.
    History {
        name: Option<String>,
        #[arg(long)]
        run: Option<String>,
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
                Some(Command::Attach { name }) => tui::attach(paths, name).await,
                Some(Command::History { name, run }) => {
                    let target = client::target(name, std::env::current_dir()?);
                    print_history(&paths, target, run).await
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

async fn print_history(paths: &ServedPaths, target: Target, run: Option<String>) -> Result<()> {
    if let Some(id) = run {
        let mut offset = 0_u64;
        loop {
            let response = client::request(
                paths,
                Request::HistoryChunk {
                    target: target.clone(),
                    id: id.clone(),
                    offset,
                    limit: served::logs::DEFAULT_CHUNK_LIMIT,
                },
            )
            .await?;
            let Response::HistoryChunk {
                next_offset,
                eof,
                content,
                ..
            } = response
            else {
                bail!("unexpected manager response")
            };
            print!("{content}");
            if eof || next_offset <= offset {
                break;
            }
            offset = next_offset;
        }
        return Ok(());
    }

    let response = client::request(paths, Request::HistoryList { target }).await?;
    let Response::HistoryList { records, .. } = response else {
        bail!("unexpected manager response")
    };
    for record in records {
        println!(
            "{:<28} {:>10} bytes  {}",
            record.id,
            record.bytes,
            if record.persisted { "disk" } else { "memory" }
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn history_accepts_optional_name_and_run_selector() {
        let command =
            Cli::try_parse_from(["served", "history", "api", "--run", "20260724-233045.log"])
                .expect("parse history");
        assert!(matches!(
            command.command,
            Some(Command::History {
                name: Some(name),
                run: Some(run),
            }) if name == "api" && run == "20260724-233045.log"
        ));
    }
}
