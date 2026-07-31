use std::{path::Path, process::ExitStatus};

use anyhow::{Context, Result, bail};
use tokio::process::Command;

pub fn resolve(explicit: Option<String>) -> Result<String> {
    explicit
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no editor configured; set EDITOR or use -e/--editor COMMAND where supported"
            )
        })
}

pub async fn run(editor: &str, path: &Path) -> Result<ExitStatus> {
    let command = format!("exec {editor} {}", shell_quote(&path.to_string_lossy()));
    Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .status()
        .await
        .context("run external editor")
}

pub fn require_success(status: ExitStatus) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    if let Some(code) = status.code() {
        bail!("external editor exited with status {code}");
    }
    bail!("external editor terminated by signal")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_keeps_path_as_one_argument() {
        assert_eq!(shell_quote("/tmp/log's path"), "'/tmp/log'\\''s path'");
    }

    #[test]
    fn explicit_editor_does_not_require_environment_configuration() {
        assert_eq!(
            resolve(Some("nvim -f".to_owned())).expect("explicit editor"),
            "nvim -f"
        );
    }
}
