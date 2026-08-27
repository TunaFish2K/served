use std::{fs, process::Command};

use tempfile::tempdir;

const CURRENT_CONFIG: &str = ".served.json5";
const LEGACY_CONFIG: &str = ".served.json";

#[test]
fn edit_path_creates_current_config_without_warning() {
    let directory = tempdir().expect("tempdir");

    let output = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["edit", "--path"])
        .current_dir(directory.path())
        .output()
        .expect("run served edit --path");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("utf8 stdout"),
        format!("{}\n", directory.path().join(CURRENT_CONFIG).display())
    );
    assert!(output.stderr.is_empty());
    assert!(directory.path().join(CURRENT_CONFIG).is_file());
    assert!(!directory.path().join(LEGACY_CONFIG).exists());
}

#[test]
fn edit_path_uses_deprecated_config_and_warns_only_on_stderr() {
    let directory = tempdir().expect("tempdir");
    fs::write(
        directory.path().join(LEGACY_CONFIG),
        r#"{name: "legacy", command: "echo ok"}"#,
    )
    .expect("legacy config");

    let output = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["edit", "--path"])
        .current_dir(directory.path())
        .output()
        .expect("run served edit --path");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("utf8 stdout"),
        format!("{}\n", directory.path().join(LEGACY_CONFIG).display())
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("warning:"));
    assert!(stderr.contains("is deprecated"));
    assert!(!directory.path().join(CURRENT_CONFIG).exists());
}

#[test]
fn edit_path_prefers_current_config_and_warns_that_legacy_is_ignored() {
    let directory = tempdir().expect("tempdir");
    fs::write(
        directory.path().join(LEGACY_CONFIG),
        r#"{name: "legacy", command: "echo legacy"}"#,
    )
    .expect("legacy config");
    fs::write(
        directory.path().join(CURRENT_CONFIG),
        r#"{name: "current", command: "echo current"}"#,
    )
    .expect("current config");

    let output = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["edit", "--path"])
        .current_dir(directory.path())
        .output()
        .expect("run served edit --path");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("utf8 stdout"),
        format!("{}\n", directory.path().join(CURRENT_CONFIG).display())
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("warning:"));
    assert!(stderr.contains("ignoring deprecated"));
}
