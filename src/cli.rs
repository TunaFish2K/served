use std::{
    collections::BTreeMap,
    io::{ErrorKind, IsTerminal},
    path::{Path, PathBuf},
    process,
};

use crate::{
    attach, client,
    config::{
        DEFAULT_LOG_MAX_BYTES, DEFAULT_LOG_MAX_FILES, default_service_name, prepare_config_file,
        prepare_explicit_config,
    },
    editor,
    logs::DEFAULT_CHUNK_LIMIT,
    manager,
    paths::ServedPaths,
    protocol::{HistoryRecord, Request, Response, RunSpec, ServiceKind, Target},
    runner,
};
use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
mod output;
use serde::Serialize;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tracing_subscriber::EnvFilter;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Parser)]
#[command(
    name = "served",
    disable_version_flag = true,
    about = "lightweight per-user service manager"
)]
struct Cli {
    /// Print build version and capabilities (same as the version command).
    #[arg(short = 'V', long)]
    version: bool,
    /// Output format for one-shot commands (JSON schema version 1).
    #[arg(long, global = true, value_enum)]
    output: Option<OutputFormat>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print build version and capabilities (same as --version).
    Version,
    /// Run the manager in the foreground under a process supervisor.
    Daemon {
        /// Ask the running manager to replace itself while keeping runners alive.
        #[arg(long, conflicts_with = "relinquish")]
        handoff: bool,
        /// Ask the running manager to exit while keeping runners alive.
        #[arg(long, conflicts_with = "handoff")]
        relinquish: bool,
    },
    /// Stop the manager and all managed runners.
    Shutdown,
    /// Replace the manager process while keeping runners alive.
    #[command(hide = true)]
    Runner {
        #[arg(long)]
        name: String,
        #[arg(long)]
        socket: std::path::PathBuf,
    },
    /// Open a service configuration in an external editor.
    Edit {
        /// Configuration path; defaults to .served.json5 or legacy .served.json.
        #[arg(short = 'f', long, value_name = "PATH")]
        file: Option<PathBuf>,
        #[arg(short = 'e', long, value_name = "COMMAND", conflicts_with = "path")]
        editor: Option<String>,
        #[arg(long, conflicts_with = "editor")]
        path: bool,
    },
    /// Enable a service configuration and start it.
    Enable {
        /// Explicit configuration path, parsed as JSON5 regardless of filename.
        #[arg(short = 'f', long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Working directory override, saved for subsequent restarts.
        #[arg(long, value_name = "DIR")]
        workdir: Option<PathBuf>,
    },
    /// Create a temporary service from command-line options and start it.
    Run {
        /// Process working directory. Defaults to the invocation directory.
        #[arg(long, value_name = "DIR")]
        workdir: Option<PathBuf>,
        /// Service name. Defaults to the working directory name.
        #[arg(long)]
        name: Option<String>,
        /// Use pipes instead of allocating a PTY.
        #[arg(long)]
        no_tty: bool,
        /// Keep the initial PTY size instead of following an attach client.
        #[arg(long)]
        no_sync_rows_cols: bool,
        /// Restart policy after the process exits.
        #[arg(long, default_value = "never", value_parser = ["never", "on-failure", "always"])]
        restart: String,
        /// Persist complete raw output under the served state directory.
        #[arg(long)]
        persist_logs: bool,
        /// Maximum bytes in one persistent log segment.
        #[arg(long, default_value_t = DEFAULT_LOG_MAX_BYTES)]
        log_max_bytes: u64,
        /// Number of archived persistent log segments to retain.
        #[arg(long, default_value_t = DEFAULT_LOG_MAX_FILES)]
        log_max_files: u32,
        /// Add or replace one literal environment value.
        #[arg(long, value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// Program and arguments. Use an explicit `sh -c` for shell syntax.
        #[arg(last = true, required = true, num_args = 1..)]
        argv: Vec<String>,
    },
    /// Disable the current service, or a managed service by name.
    Disable { name: Option<String> },
    /// Restart the current service, or a managed service by name.
    Restart { name: Option<String> },
    /// Start a stopped managed service without changing a running service.
    Start { name: Option<String> },
    /// Stop a service while retaining its registration and history.
    Stop { name: Option<String> },
    /// Attach directly to the current service, or a managed service by name.
    Attach {
        name: Option<String>,
        /// Transfer raw bytes without terminal setup or prompts.
        #[arg(long)]
        stream: bool,
        /// Receive output without reading stdin.
        #[arg(long)]
        no_stdin: bool,
    },
    /// Read service output history, open its raw file, or print its path.
    History {
        name: Option<String>,
        /// Enumerate history records instead of reading one record.
        #[arg(long, conflicts_with_all = ["run", "editor", "path", "stdout", "json"])]
        list: bool,
        #[arg(long)]
        run: Option<String>,
        #[arg(
            short = 'e',
            long,
            value_name = "COMMAND",
            conflicts_with_all = ["path", "stdout", "json"]
        )]
        editor: Option<String>,
        #[arg(long, conflicts_with_all = ["editor", "stdout", "json"])]
        path: bool,
        /// Write sanitized history content to stdout.
        #[arg(long, conflicts_with_all = ["editor", "path", "json"])]
        stdout: bool,
        /// Write sanitized history content and metadata as JSON.
        #[arg(long, conflicts_with_all = ["editor", "path", "stdout"])]
        json: bool,
    },
    /// Print services known to the manager.
    List,
}

#[derive(Debug, thiserror::Error)]
#[error("command exited with status {0}")]
pub struct ReportedExit(pub i32);

pub async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .try_init()
        .ok();
    let args: Vec<_> = std::env::args_os().collect();
    let mut cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => {
            if error.kind() == clap::error::ErrorKind::DisplayHelp {
                print_text(&error.to_string()).await?;
                return Ok(());
            }
            let json = args
                .iter()
                .take_while(|arg| *arg != "--")
                .collect::<Vec<_>>();
            let json = json.iter().any(|arg| **arg == "--output=json")
                || json
                    .windows(2)
                    .any(|pair| pair[0] == "--output" && pair[1] == "json");
            return report_failure(json, "invalid_arguments", error.to_string(), 2).await;
        }
    };
    let json = cli.output == Some(OutputFormat::Json);
    if cli.version {
        if !matches!(cli.command, None | Some(Command::Version)) {
            return report_failure(
                json,
                "invalid_arguments",
                "--version cannot be combined with another command".to_owned(),
                2,
            )
            .await;
        }
        cli.command = Some(Command::Version);
    }
    if let Err(message) = validate(&cli) {
        return report_failure(json, "invalid_arguments", message.to_owned(), 2).await;
    }
    match execute(cli.command, json).await {
        Ok(data) => {
            if json {
                print_json(&output::Document::success(data)).await?;
            }
            Ok(())
        }
        Err(error) if error.is::<attach::Interrupted>() => Err(error),
        Err(error) => report_failure(json, "operation_failed", format!("{error:#}"), 1).await,
    }
}

fn validate(cli: &Cli) -> std::result::Result<(), &'static str> {
    let json = cli.output == Some(OutputFormat::Json);
    if let Some(Command::History { json: true, .. }) = &cli.command {
        if cli.output.is_some() {
            return Err("history --json conflicts with --output; use one output interface");
        }
    }
    if json {
        match &cli.command {
            None | Some(Command::Attach { .. } | Command::Runner { .. }) => {
                return Err("--output json requires a one-shot command; attach uses raw bytes");
            }
            Some(Command::Daemon {
                handoff: false,
                relinquish: false,
            }) => return Err("foreground daemon does not support --output json"),
            Some(Command::Edit { path: false, .. }) => {
                return Err("JSON edit requires --path; editors cannot run in JSON mode");
            }
            Some(
                Command::History {
                    editor: Some(_), ..
                }
                | Command::History { stdout: true, .. },
            ) => return Err("JSON history conflicts with --editor and --stdout"),
            _ => {}
        }
    }
    if (!std::io::stdin().is_terminal() || !std::io::stdout().is_terminal())
        && matches!(
            &cli.command,
            Some(Command::Edit {
                path: false,
                editor: None,
                ..
            })
        )
    {
        return Err("non-interactive edit requires --path or an explicit --editor");
    }
    Ok(())
}

async fn report_failure(
    json: bool,
    code: &'static str,
    message: String,
    status: i32,
) -> Result<()> {
    if json {
        print_json(&output::Document::failure(code, message)).await?;
    } else {
        eprintln!("error: {message}");
    }
    Err(ReportedExit(status).into())
}

async fn print_json(value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec(value).context("encode CLI JSON")?;
    bytes.push(b'\n');
    print_bytes(&bytes).await
}
async fn print_text(value: &str) -> Result<()> {
    print_bytes(value.as_bytes()).await
}
async fn print_bytes(value: &[u8]) -> Result<()> {
    let mut out = tokio::io::stdout();
    if write_output(&mut out, value).await? {
        flush_output(&mut out).await?;
    }
    Ok(())
}

async fn execute(command: Option<Command>, json_output: bool) -> Result<Value> {
    match command {
        None => {
            #[cfg(feature = "tui")]
            if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                crate::tui::run(ServedPaths::from_environment()?).await?;
                return Ok(json!({}));
            }
            print_text(&Cli::command().render_help().to_string()).await?;
            return Ok(json!({}));
        }
        Some(Command::Version) => {
            let variant = if cfg!(feature = "tui") {
                "full"
            } else {
                "headless"
            };
            let features: &[&str] = if cfg!(feature = "tui") { &["tui"] } else { &[] };
            if !json_output {
                print_text(&format!(
                    "served {} ({variant})\n",
                    env!("CARGO_PKG_VERSION")
                ))
                .await?;
            }
            return Ok(
                json!({"version": env!("CARGO_PKG_VERSION"), "variant": variant, "features": features}),
            );
        }
        Some(Command::Edit { file, editor, path }) => {
            let directory = std::env::current_dir().context("read current directory")?;
            return edit_config(&directory, file.as_deref(), editor, path, json_output).await;
        }
        _ => {}
    }
    let paths = ServedPaths::from_environment().context("served requires HOME")?;
    match command {
        Some(Command::Daemon {
            handoff,
            relinquish,
        }) => {
            if handoff {
                manager::request_handoff(paths).await?;
            } else if relinquish {
                manager::request_relinquish(paths).await?;
            } else if matches!(
                manager::run_daemon(paths).await?,
                manager::DaemonExit::Relinquished
            ) {
                process::exit(manager::SUPERVISOR_RELINQUISH_EXIT_CODE);
            }
        }
        Some(Command::Shutdown) => manager::request_shutdown(paths).await?,
        Some(Command::Runner { name, socket }) => runner::run(name, socket).await?,
        Some(Command::Enable { file, workdir }) => {
            let current = std::env::current_dir().context("read current directory")?;
            let workdir = workdir.map(|path| current.join(path));
            let directory = workdir.as_deref().unwrap_or(&current);
            client::expect_ok(
                &paths,
                Request::Enable {
                    directory: directory.display().to_string(),
                    file: file.map(|path| current.join(path).display().to_string()),
                    workdir: workdir.as_ref().map(|path| path.display().to_string()),
                },
            )
            .await?;
        }
        Some(Command::Run {
            workdir,
            name,
            no_tty,
            no_sync_rows_cols,
            restart,
            persist_logs,
            log_max_bytes,
            log_max_files,
            env,
            argv,
        }) => {
            let directory = std::env::current_dir().context("read current directory")?;
            let directory = crate::config::canonical_directory(
                &workdir
                    .map(|path| directory.join(path))
                    .unwrap_or(directory),
            )?;
            let name = name.unwrap_or_else(|| default_service_name(&directory));
            let spec = RunSpec {
                directory: directory.display().to_string(),
                name: name.clone(),
                argv,
                tty: !no_tty,
                sync_rows_cols: !no_sync_rows_cols,
                restart,
                persist_logs,
                log_max_bytes,
                log_max_files,
                env: parse_environment(env)?,
            };
            client::expect_ok(&paths, Request::Run { spec }).await?;
            if !json_output {
                print_text(&format!("{name}\n")).await?;
            }
            return Ok(json!({"name": name}));
        }
        Some(Command::Disable { name }) => {
            client::expect_ok(
                &paths,
                Request::Disable {
                    target: client::target(name, std::env::current_dir()?),
                },
            )
            .await?
        }
        Some(Command::Restart { name }) => {
            client::expect_ok(
                &paths,
                Request::Restart {
                    target: client::target(name, std::env::current_dir()?),
                },
            )
            .await?
        }
        Some(Command::Start { name }) => {
            client::expect_ok(
                &paths,
                Request::Start {
                    target: client::target(name, std::env::current_dir()?),
                },
            )
            .await?
        }
        Some(Command::Stop { name }) => {
            client::expect_ok(
                &paths,
                Request::Stop {
                    target: client::target(name, std::env::current_dir()?),
                },
            )
            .await?
        }
        Some(Command::Attach {
            name,
            stream,
            no_stdin,
        }) => attach::attach(paths, name, stream, no_stdin).await?,
        Some(Command::List) => return print_list(&paths, json_output).await,
        Some(Command::History {
            name,
            run,
            editor,
            path,
            stdout,
            json,
            list,
        }) => {
            let target = client::target(name, std::env::current_dir()?);
            return print_history(
                &paths,
                target,
                HistoryOptions {
                    run,
                    editor,
                    path_only: path,
                    stdout,
                    json,
                    list,
                },
                json_output,
            )
            .await;
        }
        None | Some(Command::Edit { .. } | Command::Version) => unreachable!("handled above"),
    }
    Ok(json!({}))
}

async fn print_list(paths: &ServedPaths, json_output: bool) -> Result<Value> {
    let response = client::request(paths, Request::List).await?;
    let Response::Services { services } = response else {
        bail!("unexpected manager response")
    };
    let data = json!({"services": services.iter().map(output::Service::from).collect::<Vec<_>>()});
    let mut text = String::new();
    for service in services {
        text.push_str(&format!(
            "{:<18} {:<11} kind={:<9} pid={:<7} tty={} restart={} {}\n",
            service.name,
            format_state(&service.state),
            format_kind(&service.kind),
            service
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            service.tty,
            service.restart,
            service.directory
        ));
    }
    if !json_output {
        print_text(&text).await?;
    }
    Ok(data)
}

fn parse_environment(values: Vec<String>) -> Result<BTreeMap<String, String>> {
    let mut environment = BTreeMap::new();
    for value in values {
        let (key, value) = value
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid --env value; expected KEY=VALUE"))?;
        if key.is_empty() || key.contains('\0') {
            bail!("invalid environment key {key:?}");
        }
        environment.insert(key.to_owned(), value.to_owned());
    }
    Ok(environment)
}

struct HistoryOptions {
    run: Option<String>,
    editor: Option<String>,
    path_only: bool,
    stdout: bool,
    json: bool,
    list: bool,
}

async fn print_history(
    paths: &ServedPaths,
    target: Target,
    options: HistoryOptions,
    json_output: bool,
) -> Result<Value> {
    let HistoryOptions {
        run,
        editor,
        path_only,
        stdout,
        json,
        list,
    } = options;
    let response = client::request(
        paths,
        Request::HistoryList {
            target: target.clone(),
        },
    )
    .await?;
    let Response::HistoryList { service, records } = response else {
        bail!("unexpected manager response")
    };
    if list {
        if !json_output {
            let text = records
                .iter()
                .map(|r| {
                    format!(
                        "{} {} B {}{}\n",
                        r.id,
                        r.bytes,
                        if r.persisted { "disk" } else { "memory" },
                        if r.current { " current" } else { "" }
                    )
                })
                .collect::<String>();
            print_text(&text).await?;
        }
        return Ok(
            json!({"service": service, "records": records.iter().map(output::Record::from).collect::<Vec<_>>()}),
        );
    }
    let id = run.unwrap_or_else(|| "latest".to_owned());
    let record = records
        .into_iter()
        .find(|record| record.id == id)
        .ok_or_else(|| anyhow::anyhow!("history record {id:?} was not found"))?;
    if stdout
        || (!json_output
            && !json
            && !path_only
            && editor.is_none()
            && (!std::io::stdin().is_terminal() || !std::io::stdout().is_terminal()))
    {
        let mut output = tokio::io::stdout();
        write_history_text(paths, target, &service, &record, &mut output).await?;
        return Ok(json!({}));
    }
    if json || (json_output && !path_only) {
        let content = read_history_content(paths, target, &service, &record).await?;
        let document = HistoryJson {
            service: &service,
            id: &record.id,
            current: record.current,
            persisted: record.persisted,
            raw_bytes: record.bytes,
            total_lines: content.lines().count() as u64,
            content: &content,
        };
        if json_output {
            return Ok(serde_json::to_value(document)?);
        }
        print_json(&document).await?;
        return Ok(json!({}));
    }
    if !record.persisted {
        bail!(
            "history record {id:?} is memory-only and has no path; use --stdout or --json to read it"
        );
    }

    let file_name = if id == "latest" {
        "latest.log"
    } else {
        id.as_str()
    };
    let path = paths.logs_dir().join(service).join(file_name);
    if path_only {
        if !json_output {
            print_text(&format!("{}\n", path.display())).await?;
        }
        return Ok(json!({"path": path}));
    }

    let editor = editor::resolve(editor)?;
    open_editor_or_exit(&editor, &path).await?;
    Ok(json!({}))
}

#[derive(Serialize)]
struct HistoryJson<'a> {
    service: &'a str,
    id: &'a str,
    current: bool,
    persisted: bool,
    raw_bytes: u64,
    total_lines: u64,
    content: &'a str,
}

struct HistoryPage {
    next_offset: u64,
    eof: bool,
    content: String,
}

async fn write_history_text<W>(
    paths: &ServedPaths,
    target: Target,
    service: &str,
    record: &HistoryRecord,
    output: &mut W,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut offset = 0_u64;
    while offset < record.bytes {
        let page = history_page(paths, &target, service, &record.id, offset, record.bytes).await?;
        if !write_output(output, page.content.as_bytes()).await? {
            return Ok(());
        }
        if page.eof {
            break;
        }
        if page.next_offset <= offset {
            bail!("history reader made no progress")
        }
        offset = page.next_offset;
    }
    flush_output(output).await
}

async fn read_history_content(
    paths: &ServedPaths,
    target: Target,
    service: &str,
    record: &HistoryRecord,
) -> Result<String> {
    let mut content = String::new();
    let mut offset = 0_u64;
    while offset < record.bytes {
        let page = history_page(paths, &target, service, &record.id, offset, record.bytes).await?;
        content.push_str(&page.content);
        if page.eof {
            break;
        }
        if page.next_offset <= offset {
            bail!("history reader made no progress")
        }
        offset = page.next_offset;
    }
    Ok(content)
}

async fn history_page(
    paths: &ServedPaths,
    target: &Target,
    service: &str,
    id: &str,
    offset: u64,
    snapshot_bytes: u64,
) -> Result<HistoryPage> {
    let remaining = snapshot_bytes.saturating_sub(offset);
    let limit = remaining.min(u64::from(DEFAULT_CHUNK_LIMIT)) as u32;
    let response = client::request(
        paths,
        Request::HistoryChunk {
            target: target.clone(),
            id: id.to_owned(),
            offset,
            limit,
        },
    )
    .await?;
    let Response::HistoryChunk {
        service: actual_service,
        id: actual_id,
        offset: actual_offset,
        next_offset,
        eof,
        content,
        ..
    } = response
    else {
        bail!("unexpected manager response")
    };
    if actual_service != service || actual_id != id || actual_offset != offset {
        bail!("history response does not match the selected record")
    }
    if next_offset > snapshot_bytes || next_offset < offset {
        bail!("history response contains an invalid offset")
    }
    Ok(HistoryPage {
        next_offset,
        eof,
        content,
    })
}

async fn write_output<W>(output: &mut W, bytes: &[u8]) -> Result<bool>
where
    W: AsyncWrite + Unpin,
{
    match output.write_all(bytes).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(false),
        Err(error) => Err(error).context("write history output"),
    }
}

async fn flush_output<W>(output: &mut W) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    match output.flush().await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("flush history output"),
    }
}

async fn edit_config(
    directory: &Path,
    file: Option<&Path>,
    editor: Option<String>,
    path_only: bool,
    json_output: bool,
) -> Result<Value> {
    let config_file = match file {
        Some(file) => prepare_explicit_config(&directory.join(file)),
        None => prepare_config_file(directory),
    }
    .context("prepare service configuration")?;
    if let Some(warning) = config_file.deprecation_warning() {
        eprintln!("warning: {warning}");
    }
    let path = config_file.path();
    if path_only {
        if !json_output {
            print_text(&format!("{}\n", path.display())).await?;
        }
        return Ok(json!({"path": path}));
    }

    let editor = editor::resolve(editor)?;
    open_editor_or_exit(&editor, path).await?;
    Ok(json!({}))
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

fn format_state(state: &crate::protocol::ServiceState) -> &'static str {
    match state {
        crate::protocol::ServiceState::Starting => "starting",
        crate::protocol::ServiceState::Running => "running",
        crate::protocol::ServiceState::Restarting => "restarting",
        crate::protocol::ServiceState::Stopped => "stopped",
        crate::protocol::ServiceState::Failed => "failed",
    }
}

fn format_kind(kind: &ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Enabled => "enabled",
        ServiceKind::Temporary => "temporary",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn start_stop_accept_names_or_current_directory_but_not_config_files() {
        for verb in ["start", "stop"] {
            assert!(Cli::try_parse_from(["served", verb]).is_ok());
            assert!(Cli::try_parse_from(["served", verb, "api"]).is_ok());
            assert!(Cli::try_parse_from(["served", verb, "-f", "api.json5"]).is_err());
        }
    }

    #[test]
    fn program_version_flags_are_preserved_after_separator() {
        let cli = Cli::try_parse_from([
            "served",
            "run",
            "--",
            "program",
            "-V",
            "--version",
            "--output",
            "json",
        ])
        .unwrap();
        assert!(!cli.version);
        assert!(cli.output.is_none());
        let Some(Command::Run { argv, .. }) = cli.command else {
            panic!("run expected")
        };
        assert_eq!(argv, ["program", "-V", "--version", "--output", "json"]);
    }

    #[test]
    fn attach_accepts_an_optional_name() {
        let without_name = Cli::try_parse_from(["served", "attach"]).expect("parse attach");
        assert!(matches!(
            without_name.command,
            Some(Command::Attach { name: None, .. })
        ));

        let with_name =
            Cli::try_parse_from(["served", "attach", "api"]).expect("parse named attach");
        assert!(matches!(
            with_name.command,
            Some(Command::Attach { name: Some(name), .. }) if name == "api"
        ));
    }

    #[test]
    fn run_requires_an_argv_and_accepts_full_service_options() {
        let command = Cli::try_parse_from([
            "served",
            "run",
            "--name",
            "worker",
            "--no-tty",
            "--no-sync-rows-cols",
            "--restart",
            "on-failure",
            "--persist-logs",
            "--log-max-bytes",
            "1024",
            "--log-max-files",
            "4",
            "--env",
            "PORT=8080",
            "--env",
            "EMPTY=",
            "--",
            "printf",
            "%s",
            "hello world",
        ])
        .expect("parse run");
        assert!(matches!(
            command.command,
            Some(Command::Run {
                name: Some(name),
                no_tty: true,
                no_sync_rows_cols: true,
                restart,
                persist_logs: true,
                log_max_bytes: 1024,
                log_max_files: 4,
                env,
                argv,
                workdir: None,
            }) if name == "worker"
                && restart == "on-failure"
                && env == ["PORT=8080", "EMPTY="]
                && argv == ["printf", "%s", "hello world"]
        ));
        assert!(Cli::try_parse_from(["served", "run"]).is_err());
        assert!(Cli::try_parse_from(["served", "run", "printf"]).is_err());
        assert!(
            Cli::try_parse_from(["served", "run", "--restart", "sometimes", "--", "true"]).is_err()
        );
    }

    #[test]
    fn run_environment_values_are_literal_and_last_duplicate_wins() {
        let environment = parse_environment(vec![
            "A=first".to_owned(),
            "A=last=value".to_owned(),
            "EMPTY=".to_owned(),
        ])
        .expect("parse environment");
        assert_eq!(environment.get("A").map(String::as_str), Some("last=value"));
        assert_eq!(environment.get("EMPTY").map(String::as_str), Some(""));
        assert!(parse_environment(vec!["MISSING".to_owned()]).is_err());
        assert!(parse_environment(vec!["=value".to_owned()]).is_err());
    }

    #[test]
    fn supervisor_lifecycle_commands_are_public() {
        let daemon =
            Cli::try_parse_from(["served", "daemon", "--handoff"]).expect("parse daemon handoff");
        assert!(matches!(
            daemon.command,
            Some(Command::Daemon {
                handoff: true,
                relinquish: false,
            })
        ));

        let relinquish =
            Cli::try_parse_from(["served", "daemon", "--relinquish"]).expect("parse relinquish");
        assert!(matches!(
            relinquish.command,
            Some(Command::Daemon {
                handoff: false,
                relinquish: true,
            })
        ));
        assert!(Cli::try_parse_from(["served", "daemon", "--handoff", "--relinquish"]).is_err());

        let shutdown = Cli::try_parse_from(["served", "shutdown"]).expect("parse shutdown");
        assert!(matches!(shutdown.command, Some(Command::Shutdown)));
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
                file: None,
            }) if editor == "nvim -f"
        ));

        let path = Cli::try_parse_from(["served", "edit", "--path"]).expect("parse edit path");
        assert!(matches!(
            path.command,
            Some(Command::Edit {
                editor: None,
                path: true,
                file: None,
            })
        ));
        assert!(Cli::try_parse_from(["served", "edit", "--path", "-e", "nvim"]).is_err());
    }

    #[tokio::test]
    async fn edit_path_creates_template_without_editor() {
        let directory = tempdir().expect("tempdir");

        edit_config(directory.path(), None, None, true, false)
            .await
            .expect("create config path");

        let path = directory.path().join(crate::config::CONFIG_FILE);
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
                stdout: false,
                json: false,
                list: false,
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

        let stdout = Cli::try_parse_from(["served", "history", "api", "--stdout"])
            .expect("parse history stdout");
        assert!(matches!(
            stdout.command,
            Some(Command::History {
                stdout: true,
                json: false,
                ..
            })
        ));

        let json = Cli::try_parse_from(["served", "history", "api", "--json"])
            .expect("parse history JSON");
        assert!(matches!(
            json.command,
            Some(Command::History {
                stdout: false,
                json: true,
                ..
            })
        ));
        assert!(Cli::try_parse_from(["served", "history", "--stdout", "--json"]).is_err());
        assert!(Cli::try_parse_from(["served", "history", "--stdout", "--path"]).is_err());
        assert!(Cli::try_parse_from(["served", "history", "--json", "--editor", "nvim"]).is_err());
    }
}
