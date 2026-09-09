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
        format!(
            "{}\n",
            fs::canonicalize(directory.path())
                .expect("canonical tempdir")
                .join(CURRENT_CONFIG)
                .display()
        )
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
        format!(
            "{}\n",
            fs::canonicalize(directory.path())
                .expect("canonical tempdir")
                .join(LEGACY_CONFIG)
                .display()
        )
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
        format!(
            "{}\n",
            fs::canonicalize(directory.path())
                .expect("canonical tempdir")
                .join(CURRENT_CONFIG)
                .display()
        )
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(stderr.contains("warning:"));
    assert!(stderr.contains("ignoring deprecated"));
}

#[test]
fn explicit_file_creates_parents_and_preserves_existing_source() {
    let root = tempdir().unwrap();
    let relative = "configs/custom.service";
    let output = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["edit", "-f", relative, "--path"])
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let path = root.path().join(relative);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        fs::canonicalize(&path).unwrap().to_str().unwrap()
    );
    assert!(output.stderr.is_empty());
    assert!(fs::read_to_string(&path).unwrap().contains("cwd: null"));
    fs::write(&path, "// keep even invalid source\n{").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["edit", "--file", relative, "--path"])
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "// keep even invalid source\n{"
    );
}

#[test]
fn explicit_legacy_file_bypasses_discovery_and_deprecation() {
    let root = tempdir().unwrap();
    fs::write(root.path().join(CURRENT_CONFIG), "invalid").unwrap();
    fs::write(root.path().join(LEGACY_CONFIG), "{}").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["edit", "-f", LEGACY_CONFIG, "--path"])
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        fs::canonicalize(root.path().join(LEGACY_CONFIG))
            .unwrap()
            .to_str()
            .unwrap()
    );
}
