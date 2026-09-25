use serde_json::{Value, json};
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_served"))
        .args(args)
        .env_remove("HOME")
        .output()
        .unwrap()
}
fn document(output: &Output, status: i32) -> Value {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).expect("one JSON document");
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["ok"], status == 0);
    assert_eq!(output.stdout.last(), Some(&b'\n'));
    assert!(output.stderr.is_empty(), "{output:?}");
    value
}
#[test]
fn version_and_help_do_not_require_home_or_a_manager() {
    for args in [
        vec!["version", "--output", "json"],
        vec!["--output=json", "version"],
        vec!["-V", "--output", "json"],
        vec!["--output", "json", "-V"],
        vec!["--version", "--output=json"],
        vec!["--output=json", "--version"],
        vec!["-V", "version", "--output=json"],
    ] {
        let value = document(&cli(&args), 0);
        assert_eq!(value["data"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            value["data"]["variant"],
            if cfg!(feature = "tui") {
                "full"
            } else {
                "headless"
            }
        );
        assert_eq!(
            value["data"]["features"],
            if cfg!(feature = "tui") {
                json!(["tui"])
            } else {
                json!([])
            }
        );
    }
    let variant = if cfg!(feature = "tui") {
        "full"
    } else {
        "headless"
    };
    let expected = format!("served {} ({variant})\n", env!("CARGO_PKG_VERSION"));
    for args in [
        vec!["-V"],
        vec!["--version"],
        vec!["version"],
        vec!["--version", "version"],
        vec!["--output=text", "-V"],
    ] {
        let output = cli(&args);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, expected.as_bytes());
        assert!(output.stderr.is_empty());
    }
    for args in [vec![], vec!["--help"], vec!["--output", "json", "--help"]] {
        let output = cli(&args);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
    }
}
#[test]
fn json_parameter_errors_are_structured_and_reject_interactive_commands_early() {
    for args in [
        vec!["--output", "json"],
        vec!["--version", "stop", "api", "--output=json"],
        vec!["--version", "--unknown", "--output=json"],
        vec!["version", "-V", "--output=json"],
        vec!["list", "--unknown", "--output=json"],
        vec!["--output", "json", "daemon"],
        vec!["attach", "missing", "--output", "json"],
        vec![
            "runner", "--name", "x", "--socket", "/missing", "--output", "json",
        ],
        vec!["edit", "--output", "json"],
        vec!["history", "--editor", "false", "--output", "json"],
        vec!["history", "--stdout", "--output", "json"],
        vec!["history", "--json", "--output", "json"],
        vec!["history", "--list", "--run", "latest", "--output", "json"],
    ] {
        assert_eq!(
            document(&cli(&args), 2)["error"]["code"],
            "invalid_arguments",
            "{args:?}"
        );
    }
    assert_eq!(
        document(&cli(&["list", "--output", "json"]), 1)["error"]["code"],
        "operation_failed"
    );
}
#[test]
fn noninteractive_edit_never_creates_a_file_without_explicit_intent() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("edit")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    let output = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["edit", "--path", "--output", "json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let value = document(&output, 0);
    assert!(std::path::Path::new(value["data"]["path"].as_str().unwrap()).exists());
}
#[test]
fn output_flags_after_program_separator_are_not_cli_flags() {
    let output = cli(&["run", "--", "echo", "--output", "json"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn closed_output_pipe_exits_successfully_without_panicking() {
    for entry in ["version", "-V", "--version"] {
        let (reader, writer) = nix::unistd::pipe().unwrap();
        drop(reader);
        let result = Command::new(env!("CARGO_BIN_EXE_served"))
            .args([entry, "--output=json"])
            .stdout(std::process::Stdio::from(writer))
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(result.stderr.is_empty(), "{result:?}");
    }
}
