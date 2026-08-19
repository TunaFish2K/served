use std::{ffi::OsStr, os::unix::fs::PermissionsExt, path::Path, process::ExitStatus};

use anyhow::{Context, Result, bail};
use tokio::process::Command;

const FALLBACK_EDITORS: [&str; 8] = [
    "editor",
    "sensible-editor",
    "nvim",
    "vim",
    "vi",
    "nano",
    "micro",
    "hx",
];

pub fn resolve(explicit: Option<String>) -> Result<String> {
    resolve_from(
        explicit,
        std::env::var("EDITOR").ok(),
        std::env::var_os("PATH").as_deref(),
    )
}

fn resolve_from(
    explicit: Option<String>,
    configured: Option<String>,
    path: Option<&OsStr>,
) -> Result<String> {
    if let Some(editor) = nonempty(explicit).or_else(|| nonempty(configured)) {
        return Ok(editor);
    }
    if let Some(editor) = path.and_then(find_fallback_editor) {
        return Ok(editor.to_owned());
    }
    Err(anyhow::anyhow!(
        "no editor available; use -e/--editor, set EDITOR, or install one of: {}",
        FALLBACK_EDITORS.join(", ")
    ))
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn find_fallback_editor(path: &OsStr) -> Option<&'static str> {
    FALLBACK_EDITORS.iter().copied().find(|candidate| {
        std::env::split_paths(path)
            .filter(|directory| !directory.as_os_str().is_empty())
            .any(|directory| is_executable(&directory.join(candidate)))
    })
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
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
    use std::{fs, os::unix::fs::PermissionsExt};
    use tempfile::tempdir;

    #[test]
    fn shell_quote_keeps_path_as_one_argument() {
        assert_eq!(shell_quote("/tmp/log's path"), "'/tmp/log'\\''s path'");
    }

    #[test]
    fn explicit_editor_does_not_require_environment_configuration() {
        assert_eq!(
            resolve_from(Some("nvim -f".to_owned()), None, None).expect("explicit editor"),
            "nvim -f"
        );
    }

    #[test]
    fn configured_editor_precedes_path_fallback() {
        assert_eq!(
            resolve_from(None, Some("vim -f".to_owned()), None).expect("configured editor"),
            "vim -f"
        );
    }

    #[test]
    fn fallback_prefers_system_editor_and_skips_non_executables() {
        let directory = tempdir().expect("tempdir");
        let nvim = directory.path().join("nvim");
        fs::write(&nvim, "#!/bin/sh\n").expect("write nvim");
        fs::set_permissions(&nvim, fs::Permissions::from_mode(0o755))
            .expect("make nvim executable");
        fs::write(directory.path().join("editor"), "#!/bin/sh\n").expect("write editor");

        assert_eq!(
            resolve_from(
                Some("  ".to_owned()),
                Some(String::new()),
                Some(directory.path().as_os_str()),
            )
            .expect("fallback editor"),
            "nvim"
        );

        let editor = directory.path().join("editor");
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o755))
            .expect("make editor executable");
        assert_eq!(
            resolve_from(None, None, Some(directory.path().as_os_str())).expect("system editor"),
            "editor"
        );
    }

    #[test]
    fn missing_editor_reports_every_fallback() {
        let directory = tempdir().expect("tempdir");
        let error = resolve_from(None, None, Some(directory.path().as_os_str()))
            .expect_err("missing editor");
        let message = error.to_string();
        assert!(message.contains("editor, sensible-editor, nvim, vim, vi, nano, micro, hx"));
    }
}
