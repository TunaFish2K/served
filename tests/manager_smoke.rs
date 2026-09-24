use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command},
    time::Duration,
};

use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
use portable_pty::{Child as PtyChild, CommandBuilder, PtySize, native_pty_system};
use served::{
    client,
    manager::SUPERVISOR_RELINQUISH_EXIT_CODE,
    paths::ServedPaths,
    protocol::{Request, Response, RunSpec, ServiceKind, ServiceState, Target},
};
use tempfile::{Builder, TempDir};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{sleep, timeout},
};

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn custom_sources_and_shared_workdirs_survive_recovery() {
    let root = test_root();
    let home = root.path().join("home");
    let caller = root.path().join("caller");
    let configs = root.path().join("configs");
    let project = root.path().join("project");
    let next = root.path().join("next");
    for directory in [&home, &caller, &configs, &project, &next] {
        fs::create_dir(directory).unwrap();
    }
    let paths = ServedPaths {
        config_home: home.join(".config"),
        runtime_dir: home.join(".local/state/served/runtime"),
        state_home: home.join(".local/state"),
    };
    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_served"))
            .arg("daemon")
            .env("HOME", &home)
            .spawn()
            .unwrap()
    };
    let mut daemon = DaemonGuard(spawn());
    wait_for_path(&paths.socket_path()).await;
    let cli = |args: &[&str], directory: &Path| {
        Command::new(env!("CARGO_BIN_EXE_served"))
            .args(args)
            .current_dir(directory)
            .env("HOME", &home)
            .output()
            .unwrap()
    };
    let write_config = |file: &Path, name: &str, cwd: &str, tty: bool| {
        fs::write(file, serde_json::to_vec(&serde_json::json!({
            "name":name, "command":"printf '%s|%s\\n' \"$PWD\" \"$LOCATION\"; exec sleep 60", "cwd":cwd, "tty":tty
        })).unwrap()).unwrap();
    };
    let api = configs.join("api.custom");
    let worker = configs.join("worker.custom");
    write_config(&api, "api", "../project", true);
    write_config(&worker, "worker", "does-not-exist", false);
    fs::write(configs.join(".served.json5"), "invalid").unwrap();
    fs::write(configs.join(".env.served"), "LOCATION=config").unwrap();
    fs::write(project.join(".env.served"), "LOCATION=wrong").unwrap();
    for args in [
        vec!["enable", "-f", "../configs/api.custom"],
        vec![
            "enable",
            "--file",
            "../configs/worker.custom",
            "--workdir",
            "../project",
        ],
    ] {
        let output = cli(&args, &caller);
        assert!(output.status.success(), "{output:?}");
    }
    let output = cli(
        &[
            "run",
            "--name",
            "temporary",
            "--no-tty",
            "--workdir",
            "../project",
            "--",
            "sleep",
            "60",
        ],
        &caller,
    );
    assert!(output.status.success(), "{output:?}");
    for name in ["api", "worker"] {
        wait_for_output_tail(&paths, name, &format!("{}|config", project.display())).await;
        let registration = paths.registry_dir().join(name);
        assert!(fs::symlink_metadata(&registration).unwrap().is_file());
        assert_eq!(
            fs::metadata(&registration).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(registration).unwrap()).unwrap();
        assert_eq!(
            value["source"]["path"],
            configs.join(format!("{name}.custom")).to_str().unwrap()
        );
    }
    wait_for_state(&paths, "temporary", ServiceState::Running).await;
    let names = ["api", "temporary", "worker"];
    let mut pids = Vec::new();
    for name in names {
        pids.push(service_pid(&paths, name).await);
    }
    for command in ["restart", "disable", "attach", "history"] {
        let output = cli(&[command], &project);
        assert!(!output.status.success(), "{command}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("api, temporary, worker"),
            "{output:?}"
        );
    }
    let history = cli(&["history", "worker", "--stdout"], &caller);
    assert!(history.status.success(), "{history:?}");
    assert!(String::from_utf8_lossy(&history.stdout).contains("|config"));
    for (index, name) in names.iter().enumerate() {
        assert_eq!(service_pid(&paths, name).await, pids[index]);
    }

    // Invalid reloads do not stop either service, even when they share a directory.
    fs::write(&api, "invalid").unwrap();
    assert!(!cli(&["restart", "api"], &caller).status.success());
    write_config(&api, "api", "missing", true);
    assert!(!cli(&["restart", "api"], &caller).status.success());
    assert_eq!(service_pid(&paths, "api").await, pids[0]);
    write_config(&api, "api", "../project", true);
    let duplicate = configs.join("duplicate.custom");
    write_config(&duplicate, "api", "../next", false);
    let output = cli(&["enable", "-f", duplicate.to_str().unwrap()], &caller);
    assert!(!output.status.success());
    assert_eq!(service_pid(&paths, "api").await, pids[0]);
    let output = cli(&["enable", "-f", "../configs/missing"], &caller);
    assert!(!output.status.success());

    daemon.0.kill().unwrap();
    daemon.0.wait().unwrap();
    daemon = DaemonGuard(spawn());
    for (index, name) in names.iter().enumerate() {
        wait_for_state(&paths, name, ServiceState::Running).await;
        assert_eq!(service_pid(&paths, name).await, pids[index]);
    }
    let output = cli(&["daemon", "--handoff"], &caller);
    assert!(output.status.success(), "{output:?}");
    for (index, name) in names.iter().enumerate() {
        wait_for_state(&paths, name, ServiceState::Running).await;
        assert_eq!(service_pid(&paths, name).await, pids[index]);
    }

    write_config(&api, "api", "../next", true);
    assert!(cli(&["restart", "api"], &caller).status.success());
    wait_for_output_tail(&paths, "api", &format!("{}|config", next.display())).await;
    assert_ne!(service_pid(&paths, "api").await, pids[0]);
    assert_eq!(service_pid(&paths, "worker").await, pids[2]);
    assert!(cli(&["restart", "worker"], &caller).status.success());
    wait_for_output_tail(&paths, "worker", &format!("{}|config", project.display())).await;

    assert!(cli(&["shutdown"], &caller).status.success());
    daemon.0.wait().unwrap();
    wait_for_absent(&paths.socket_path()).await;
    let _recovered = DaemonGuard(spawn());
    for (name, directory) in [("api", &next), ("worker", &project)] {
        wait_for_output_tail(&paths, name, &format!("{}|config", directory.display())).await;
    }
    let Response::Services { services } = client::request(&paths, Request::List).await.unwrap()
    else {
        panic!("list");
    };
    assert_eq!(services.len(), 2);
    assert_eq!(
        services
            .iter()
            .find(|service| service.name == "worker")
            .unwrap()
            .config_file
            .as_deref(),
        worker.to_str()
    );
    // Disable by name does not require the source to remain valid or present.
    fs::remove_file(&worker).unwrap();
    assert!(cli(&["disable", "worker"], &caller).status.success());
    assert!(!paths.registry_dir().join("worker").exists());
    assert!(cli(&["shutdown"], &caller).status.success());
}

#[tokio::test]
async fn workdir_discovery_and_legacy_sources_remain_distinct() {
    let root = test_root();
    let home = root.path().join("home");
    let project = root.path().join("project");
    let work = root.path().join("work");
    for directory in [&home, &project, &work] {
        fs::create_dir(directory).unwrap();
    }
    let paths = ServedPaths {
        config_home: home.join(".config"),
        runtime_dir: home.join(".local/state/served/runtime"),
        state_home: home.join(".local/state"),
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .unwrap();
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;
    let cli = |args: &[&str], directory: &Path| {
        Command::new(env!("CARGO_BIN_EXE_served"))
            .args(args)
            .current_dir(directory)
            .env("HOME", &home)
            .output()
            .unwrap()
    };
    let config = project.join(".served.json");
    fs::write(
        &config,
        "{name:'legacy',command:'pwd; exec sleep 60',cwd:'../work',tty:false}",
    )
    .unwrap();
    // Ordinary enable keeps the config directory link, not the process working directory.
    assert!(cli(&["enable"], &project).status.success());
    assert_eq!(
        fs::read_link(paths.registry_dir().join("legacy")).unwrap(),
        project
    );
    wait_for_output_tail(&paths, "legacy", work.to_str().unwrap()).await;
    fs::write(
        &config,
        "{name:'legacy',command:'pwd; exec sleep 60',cwd:'.',tty:false}",
    )
    .unwrap();
    assert!(cli(&["restart", "legacy"], root.path()).status.success());
    wait_for_output_tail(&paths, "legacy", project.to_str().unwrap()).await;
    assert!(cli(&["disable", "legacy"], root.path()).status.success());

    // --workdir without --file searches there and persists the override.
    fs::write(root.path().join(".served.json5"), "invalid").unwrap();
    fs::write(
        &config,
        "{name:'discovered',command:'pwd; exec sleep 60',cwd:'../work',tty:false}",
    )
    .unwrap();
    let output = cli(&["enable", "--workdir", "project"], root.path());
    assert!(output.status.success(), "{output:?}");
    wait_for_output_tail(&paths, "discovered", project.to_str().unwrap()).await;
    assert!(
        cli(&["restart", "discovered"], root.path())
            .status
            .success()
    );
    wait_for_output_tail(&paths, "discovered", project.to_str().unwrap()).await;
    assert!(
        cli(&["disable", "discovered"], root.path())
            .status
            .success()
    );
    // run derives its default name from its selected directory and ignores invalid config.
    fs::write(project.join(".served.json5"), "invalid").unwrap();
    let output = cli(
        &[
            "run",
            "--workdir",
            "project",
            "--no-tty",
            "--",
            "sh",
            "-c",
            "pwd; exec sleep 60",
        ],
        root.path(),
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"project\n");
    wait_for_output_tail(&paths, "project", project.to_str().unwrap()).await;
    assert!(cli(&["shutdown"], root.path()).status.success());
}

#[tokio::test]
async fn run_creates_a_full_temporary_service_without_reading_config_files() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("temporary-project");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(service_dir.join(".served.json5"), "{ invalid json5\n")
        .expect("invalid current config");
    fs::write(service_dir.join(".served.json"), "{ invalid json\n").expect("invalid legacy config");
    fs::write(service_dir.join(".env.served"), "FROM_FILE=must-not-load\n")
        .expect("legacy environment");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .env("MANAGER_ONLY", "daemon-value")
        .env("OVERRIDE", "daemon-value")
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;

    let script = concat!(
        "test \"$MANAGER_ONLY\" = daemon-value && ",
        "test \"$OVERRIDE\" = cli-value && ",
        "test -z \"${FROM_FILE+x}\" && ",
        "printf 'temporary-ready\\n'; sleep 60"
    );
    let run = Command::new(env!("CARGO_BIN_EXE_served"))
        .args([
            "run",
            "--name",
            "temporary",
            "--no-tty",
            "--no-sync-rows-cols",
            "--persist-logs",
            "--log-max-bytes",
            "4096",
            "--log-max-files",
            "2",
            "--env",
            "OVERRIDE=cli-value",
            "--",
            "sh",
            "-c",
            script,
        ])
        .current_dir(&service_dir)
        .env("HOME", &home)
        .env("MANAGER_ONLY", "client-value")
        .output()
        .expect("run temporary service");
    assert!(run.status.success(), "run failed: {run:?}");
    assert_eq!(run.stdout, b"temporary\n");
    wait_for_state(&paths, "temporary", ServiceState::Running).await;
    wait_for_output_tail(&paths, "temporary", "temporary-ready").await;
    let original_pid = service_pid(&paths, "temporary").await;

    let handoff = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["daemon", "--handoff"])
        .env("HOME", &home)
        .output()
        .expect("handoff temporary service manager");
    assert!(handoff.status.success(), "handoff failed: {handoff:?}");
    wait_for_state(&paths, "temporary", ServiceState::Running).await;
    assert_eq!(service_pid(&paths, "temporary").await, original_pid);

    let Response::Services { services } = client::request(&paths, Request::List)
        .await
        .expect("list services")
    else {
        panic!("unexpected list response");
    };
    let temporary = services
        .iter()
        .find(|service| service.name == "temporary")
        .expect("temporary service");
    assert_eq!(temporary.kind, ServiceKind::Temporary);
    assert!(!temporary.tty);
    assert!(temporary.persist_logs);
    assert_eq!(temporary.restart, "never");
    assert!(!paths.registry_dir().join("temporary").exists());
    assert_eq!(
        fs::metadata(paths.transient_definition("temporary"))
            .expect("transient definition")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let mut session = client::attach(&paths, "temporary".to_owned())
        .await
        .expect("attach temporary service");
    let output = read_until(&mut session.stream, b"temporary-ready").await;
    assert!(
        output
            .windows(15)
            .any(|window| window == b"temporary-ready")
    );
    drop(session);
    assert!(
        history_records(&paths, "temporary")
            .await
            .iter()
            .any(|record| record.current && record.persisted)
    );

    let collision = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["run", "--name", "other", "--", "true"])
        .current_dir(&service_dir)
        .env("HOME", &home)
        .output()
        .expect("run service in shared directory");
    assert!(
        collision.status.success(),
        "shared directory: {collision:?}"
    );
    client::expect_ok(
        &paths,
        Request::Disable {
            target: Target::Name("other".to_owned()),
        },
    )
    .await
    .expect("disable shared service");

    let other_dir = root.path().join("other-project");
    fs::create_dir(&other_dir).expect("other service directory");
    let name_collision = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["run", "--name", "temporary", "--", "true"])
        .current_dir(&other_dir)
        .env("HOME", &home)
        .output()
        .expect("run name collision");
    assert!(!name_collision.status.success());
    assert!(String::from_utf8_lossy(&name_collision.stderr).contains("already managed"));

    let restart = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("restart")
        .current_dir(&service_dir)
        .env("HOME", &home)
        .output()
        .expect("restart temporary service by directory");
    assert!(restart.status.success(), "restart failed: {restart:?}");
    wait_for_state(&paths, "temporary", ServiceState::Running).await;
    let restarted_pid = service_pid(&paths, "temporary").await;
    assert_ne!(restarted_pid, original_pid);

    let disable = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("disable")
        .current_dir(&service_dir)
        .env("HOME", &home)
        .output()
        .expect("disable temporary service by directory");
    assert!(disable.status.success(), "disable failed: {disable:?}");
    wait_for_process_exit(restarted_pid).await;
    assert!(!paths.transient_definition("temporary").exists());
    assert!(paths.logs_dir().join("temporary").exists());
}

#[tokio::test]
async fn manager_crash_preserves_a_temporary_service_for_adoption() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("temporary-adoption");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };

    let mut first = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn first manager");
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Run {
            spec: RunSpec {
                directory: service_dir.display().to_string(),
                name: "temporary-adoption".to_owned(),
                argv: vec!["sleep".to_owned(), "60".to_owned()],
                tty: false,
                sync_rows_cols: true,
                restart: "never".to_owned(),
                persist_logs: false,
                log_max_bytes: 1024,
                log_max_files: 3,
                env: Default::default(),
            },
        },
    )
    .await
    .expect("run temporary service");
    wait_for_state(&paths, "temporary-adoption", ServiceState::Running).await;
    let pid = service_pid(&paths, "temporary-adoption").await;

    first.kill().expect("kill first manager");
    first.wait().expect("reap first manager");
    assert!(process_exists(pid));
    assert!(paths.transient_definition("temporary-adoption").exists());

    let replacement = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn replacement manager");
    let _guard = DaemonGuard(replacement);
    wait_for_path(&paths.socket_path()).await;
    wait_for_state(&paths, "temporary-adoption", ServiceState::Running).await;
    assert_eq!(service_pid(&paths, "temporary-adoption").await, pid);

    let shutdown = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("shutdown")
        .env("HOME", &home)
        .output()
        .expect("shutdown replacement manager");
    assert!(shutdown.status.success(), "shutdown failed: {shutdown:?}");
    wait_for_process_exit(pid).await;
    assert!(!paths.transient_definition("temporary-adoption").exists());
}

#[tokio::test]
async fn enable_restart_and_disable_a_pipe_service() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json5"),
        r#"{
  "name": "smoke",
  "command": "test \"$SMOKE_PORT\" = \"8080\" && sleep 60",
  "tty": false,
  "restart": "never"
}
"#,
    )
    .expect("config");
    fs::write(service_dir.join(".env.served"), "SMOKE_PORT=8080\n").expect("env.served");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;

    let response = client::request(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable");
    assert!(matches!(response, Response::Ok));
    wait_for_state(&paths, "smoke", ServiceState::Running).await;

    let duplicate_dir = root.path().join("duplicate");
    fs::create_dir_all(&duplicate_dir).expect("duplicate service");
    fs::write(
        duplicate_dir.join(".served.json5"),
        r#"{
  "name": "smoke",
  "command": "exit 99",
  "tty": false,
  "restart": "never"
}
"#,
    )
    .expect("duplicate config");
    let duplicate_error = client::request(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: duplicate_dir.display().to_string(),
        },
    )
    .await
    .expect_err("duplicate service name must be rejected");
    assert!(duplicate_error.to_string().contains("already enabled"));
    assert_eq!(
        fs::read_link(paths.registry_dir().join("smoke")).expect("read enable link"),
        service_dir
    );

    let original_config = fs::read(service_dir.join(".served.json5")).expect("read config");
    fs::write(service_dir.join(".served.json5"), "{ invalid json\n").expect("invalid config");
    let error = client::request(
        &paths,
        Request::Restart {
            target: Target::Name("smoke".to_owned()),
        },
    )
    .await
    .expect_err("invalid config must reject restart");
    assert!(error.to_string().contains("invalid JSON"));
    wait_for_state(&paths, "smoke", ServiceState::Running).await;
    fs::write(service_dir.join(".served.json5"), original_config).expect("restore config");

    fs::write(service_dir.join(".env.served"), "SMOKE_PORT=9090\n").expect("new env.served");
    client::expect_ok(
        &paths,
        Request::Restart {
            target: Target::Name("smoke".to_owned()),
        },
    )
    .await
    .expect("restart with changed env");
    wait_for_state(&paths, "smoke", ServiceState::Stopped).await;

    fs::write(service_dir.join(".env.served"), "SMOKE_PORT=8080\n").expect("restored env.served");
    client::expect_ok(
        &paths,
        Request::Restart {
            target: Target::Name("smoke".to_owned()),
        },
    )
    .await
    .expect("restart");
    wait_for_state(&paths, "smoke", ServiceState::Running).await;

    client::expect_ok(
        &paths,
        Request::Disable {
            target: Target::Name("smoke".to_owned()),
        },
    )
    .await
    .expect("disable");
    assert!(!paths.registry_dir().join("smoke").exists());
}

#[tokio::test]
async fn persistent_and_memory_history_survive_service_restarts() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let persistent_dir = root.path().join("persistent");
    let memory_dir = root.path().join("memory");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&persistent_dir).expect("persistent service");
    fs::create_dir_all(&memory_dir).expect("memory service");
    fs::write(
        persistent_dir.join(".served.json"),
        r#"{
  "name": "persistent",
  "command": "printf 'persistent-output\\n'; sleep 0.2",
  "tty": false,
  "restart": "never",
  "persist_logs": true
}
"#,
    )
    .expect("persistent config");
    fs::write(
        memory_dir.join(".served.json"),
        r#"{
  "name": "memory",
  "command": "i=0; while [ \"$i\" -lt 6000 ]; do printf 'memory-line-%04d\\n' \"$i\"; i=$((i + 1)); done; printf '\\033[31mmemory-output\\033[0m\\n'; sleep 0.2",
  "tty": false,
  "restart": "never",
  "persist_logs": false
}
"#,
    )
    .expect("memory config");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;

    for directory in [&persistent_dir, &memory_dir] {
        client::expect_ok(
            &paths,
            Request::Enable {
                file: None,
                workdir: None,
                directory: directory.display().to_string(),
            },
        )
        .await
        .expect("enable history service");
    }
    wait_for_state(&paths, "persistent", ServiceState::Stopped).await;
    wait_for_state(&paths, "memory", ServiceState::Stopped).await;

    for name in ["persistent", "memory"] {
        client::expect_ok(
            &paths,
            Request::Restart {
                target: Target::Name(name.to_owned()),
            },
        )
        .await
        .expect("restart history service");
        wait_for_state(&paths, name, ServiceState::Stopped).await;
    }

    let persistent_records = history_records(&paths, "persistent").await;
    assert!(persistent_records.iter().any(|record| record.current));
    let persistent_archive = persistent_records
        .iter()
        .find(|record| !record.current && record.persisted)
        .expect("persistent archive");
    assert!(
        paths
            .logs_dir()
            .join("persistent")
            .join(&persistent_archive.id)
            .is_file()
    );
    assert_eq!(
        fs::metadata(paths.logs_dir().join("persistent"))
            .expect("persistent log directory")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(paths.logs_dir().join("persistent").join("latest.log"))
            .expect("latest log")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let persistent_output = read_history(&paths, "persistent", &persistent_archive.id, 4).await;
    assert!(persistent_output.contains("persistent-output"));
    assert!(
        paths
            .logs_dir()
            .join("persistent")
            .join("latest.log")
            .is_file()
    );

    let memory_records = history_records(&paths, "memory").await;
    assert!(memory_records.iter().any(|record| record.current));
    let memory_archive = memory_records
        .iter()
        .find(|record| !record.current && !record.persisted)
        .expect("memory archive");
    assert_eq!(memory_archive.bytes, 64 * 1024);
    assert!(!paths.logs_dir().join("memory").exists());
    let memory_output = read_history(&paths, "memory", &memory_archive.id, 4).await;
    assert!(memory_output.contains("memory-output"));

    let persistent_stdout = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["history", "persistent", "--run"])
        .arg(&persistent_archive.id)
        .arg("--stdout")
        .env("HOME", &home)
        .output()
        .expect("export persistent history");
    assert!(persistent_stdout.status.success());
    assert!(String::from_utf8_lossy(&persistent_stdout.stdout).contains("persistent-output"));

    let memory_stdout = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["history", "memory", "--run"])
        .arg(&memory_archive.id)
        .arg("--stdout")
        .env("HOME", &home)
        .output()
        .expect("export memory history");
    assert!(memory_stdout.status.success());
    let memory_stdout = String::from_utf8(memory_stdout.stdout).expect("UTF-8 history output");
    assert!(memory_stdout.contains("memory-output"));
    assert!(!memory_stdout.contains('\u{1b}'));

    let memory_json = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["history", "memory", "--run"])
        .arg(&memory_archive.id)
        .arg("--json")
        .env("HOME", &home)
        .output()
        .expect("export memory history JSON");
    assert!(memory_json.status.success());
    let memory_json: serde_json::Value =
        serde_json::from_slice(&memory_json.stdout).expect("parse history JSON");
    assert_eq!(memory_json["service"], "memory");
    assert_eq!(memory_json["id"], memory_archive.id);
    assert_eq!(memory_json["current"], false);
    assert_eq!(memory_json["persisted"], false);
    assert_eq!(memory_json["raw_bytes"], 64 * 1024);
    let memory_json_content = memory_json["content"]
        .as_str()
        .expect("JSON history content");
    assert_eq!(
        memory_json["total_lines"],
        memory_json_content.lines().count() as u64
    );
    assert!(memory_json_content.contains("memory-output"));
    assert!(!memory_json_content.contains('\u{1b}'));
    assert!(!paths.logs_dir().join("memory").exists());

    for name in ["persistent", "memory"] {
        client::expect_ok(
            &paths,
            Request::Disable {
                target: Target::Name(name.to_owned()),
            },
        )
        .await
        .expect("disable history service");
    }
}

#[tokio::test]
async fn crash_loop_attach_reports_only_a_persisted_latest_log() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let persistent_dir = root.path().join("persistent-crash");
    let memory_dir = root.path().join("memory-crash");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");

    for (directory, name, persist_logs) in [
        (&persistent_dir, "persistent-crash", true),
        (&memory_dir, "memory-crash", false),
    ] {
        fs::create_dir_all(directory).expect("service directory");
        fs::write(
            directory.join(".served.json"),
            format!(
                r#"{{
  name: "{name}",
  command: "echo {name}-output; exit 1",
  tty: false,
  restart: "always",
  persist_logs: {persist_logs},
}}
"#
            ),
        )
        .expect("crash service config");
    }

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;

    for directory in [&persistent_dir, &memory_dir] {
        client::expect_ok(
            &paths,
            Request::Enable {
                file: None,
                workdir: None,
                directory: directory.display().to_string(),
            },
        )
        .await
        .expect("enable crash service");
    }

    let persistent = wait_for_attach_unavailable(&paths, "persistent-crash").await;
    assert!(persistent.recent_failures >= 3);
    assert_eq!(persistent.window_seconds, 60);
    let latest = persistent.latest_log.expect("persistent latest log");
    assert_eq!(latest, paths.logs_dir().join("persistent-crash/latest.log"));
    assert!(latest.is_file());

    let mut direct_stderr = None;
    for _ in 0..50 {
        let output = Command::new(env!("CARGO_BIN_EXE_served"))
            .args(["attach", "persistent-crash"])
            .env("HOME", &home)
            .output()
            .expect("run non-interactive direct attach");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if stderr.contains("warning: service \"persistent-crash\"") {
            assert!(!output.status.success());
            direct_stderr = Some(stderr);
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    let direct_stderr = direct_stderr.expect("direct attach crash-loop warning");
    assert!(direct_stderr.contains("latest.log"));
    assert!(!direct_stderr.contains("Open latest.log?"));

    let memory = wait_for_attach_unavailable(&paths, "memory-crash").await;
    assert!(memory.recent_failures >= 3);
    assert_eq!(memory.window_seconds, 60);
    assert_eq!(memory.latest_log, None);
    assert!(!paths.logs_dir().join("memory-crash/latest.log").exists());

    for name in ["persistent-crash", "memory-crash"] {
        client::expect_ok(
            &paths,
            Request::Disable {
                target: Target::Name(name.to_owned()),
            },
        )
        .await
        .expect("disable crash service");
    }
}

#[tokio::test]
async fn pty_service_accepts_one_attach_session() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "pty-smoke",
  "command": "for i in $(seq 0 57); do printf 'cache-%02d\\n' \"$i\"; done; read -r line; stty size; printf 'reply:%s\\n' \"$line\"; sleep 60",
  "tty": true,
  "syncRowsCols": true,
  "restart": "never"
}
"#,
    )
    .expect("config");
    fs::write(service_dir.join(".env.served"), "").expect("env.served");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable");
    wait_for_state(&paths, "pty-smoke", ServiceState::Running).await;
    wait_for_output_tail(&paths, "pty-smoke", "cache-57").await;
    let session = client::attach(&paths, "pty-smoke".to_owned())
        .await
        .expect("attach");
    let mut resize_control = client::open_resize_control(&paths)
        .await
        .expect("open resize control");
    client::send_resize(&mut resize_control, "pty-smoke", &session.token, 120, 40)
        .await
        .expect("resize PTY");
    let mut stream = session.stream;
    stream
        .write_all(b"hello\n")
        .await
        .expect("write attach input");
    let output = read_until(&mut stream, b"reply:hello").await;
    let second_attach = client::attach(&paths, "pty-smoke".to_owned()).await;
    assert!(second_attach.is_err(), "second attach must be rejected");
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("cache-10"));
    assert!(output.contains("cache-57"));
    assert!(!output.contains("cache-09"));
    assert!(output.contains("40 120"));
    assert!(output.contains("reply:hello"));
    drop(stream);
    client::expect_ok(
        &paths,
        Request::Disable {
            target: Target::Name("pty-smoke".to_owned()),
        },
    )
    .await
    .expect("disable");
}

#[tokio::test]
async fn pipe_service_supports_multiple_readonly_attach_sessions() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "pipe-attach",
  "command": "for i in $(seq 0 57); do printf 'pipe-cache-%02d\\n' \"$i\"; done; sleep 1; while true; do printf 'pipe-live\\n'; sleep 1; done",
  "tty": false,
  "restart": "never"
}
"#,
    )
    .expect("config");
    fs::write(service_dir.join(".env.served"), "").expect("env.served");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable");
    wait_for_state(&paths, "pipe-attach", ServiceState::Running).await;
    wait_for_output_tail(&paths, "pipe-attach", "pipe-cache-57").await;

    let first_session = client::attach(&paths, "pipe-attach".to_owned())
        .await
        .expect("first read-only attach");
    let second_session = client::attach(&paths, "pipe-attach".to_owned())
        .await
        .expect("second read-only attach");
    let mut first = first_session.stream;
    let mut second = second_session.stream;
    first
        .write_all(b"ignored input\n")
        .await
        .expect("write ignored input");
    second
        .write_all(b"ignored input\n")
        .await
        .expect("write ignored input");

    let first_output = read_until(&mut first, b"pipe-live").await;
    let second_output = read_until(&mut second, b"pipe-live").await;
    assert!(
        first_output
            .windows(b"pipe-cache-10".len())
            .any(|window| window == b"pipe-cache-10")
    );
    assert!(
        second_output
            .windows(b"pipe-cache-10".len())
            .any(|window| window == b"pipe-cache-10")
    );
    assert!(
        first_output
            .windows(b"pipe-cache-57".len())
            .any(|window| window == b"pipe-cache-57")
    );
    assert!(
        second_output
            .windows(b"pipe-cache-57".len())
            .any(|window| window == b"pipe-cache-57")
    );
    assert!(
        !first_output
            .windows(b"pipe-cache-09".len())
            .any(|window| window == b"pipe-cache-09")
    );
    assert!(
        !second_output
            .windows(b"pipe-cache-09".len())
            .any(|window| window == b"pipe-cache-09")
    );
    assert!(
        first_output
            .windows(b"pipe-live".len())
            .any(|window| window == b"pipe-live")
    );
    assert!(
        second_output
            .windows(b"pipe-live".len())
            .any(|window| window == b"pipe-live")
    );

    drop(first);
    drop(second);
    wait_for_attach_state(&paths, "pipe-attach", false).await;
    client::expect_ok(
        &paths,
        Request::Disable {
            target: Target::Name("pipe-attach".to_owned()),
        },
    )
    .await
    .expect("disable");
}

#[tokio::test]
async fn process_group_stops_pipe_descendants() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("pipe-group-service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "pipe-group",
  "command": "sleep 60 & child=$!; printf '%s' \"$child\" > child.pid; wait \"$child\"",
  "tty": false,
  "restart": "never"
}
"#,
    )
    .expect("config");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable");
    wait_for_state(&paths, "pipe-group", ServiceState::Running).await;

    let child_path = service_dir.join("child.pid");
    wait_for_path(&child_path).await;
    let child_pid: u32 = fs::read_to_string(&child_path)
        .expect("read child pid")
        .parse()
        .expect("parse child pid");
    wait_for_process(child_pid).await;

    client::expect_ok(
        &paths,
        Request::Disable {
            target: Target::Name("pipe-group".to_owned()),
        },
    )
    .await
    .expect("disable");
    wait_for_process_exit(child_pid).await;
}

#[tokio::test]
async fn process_group_stops_pty_descendants() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("pty-group-service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "pty-group",
  "command": "sleep 60 & child=$!; printf '%s' \"$child\" > child.pid; wait \"$child\"",
  "tty": true,
  "restart": "never"
}
"#,
    )
    .expect("config");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable");
    wait_for_state(&paths, "pty-group", ServiceState::Running).await;

    let child_path = service_dir.join("child.pid");
    wait_for_path(&child_path).await;
    let child_pid: u32 = fs::read_to_string(&child_path)
        .expect("read child pid")
        .parse()
        .expect("parse child pid");
    wait_for_process(child_pid).await;

    client::expect_ok(
        &paths,
        Request::Disable {
            target: Target::Name("pty-group".to_owned()),
        },
    )
    .await
    .expect("disable");
    wait_for_process_exit(child_pid).await;
}

#[tokio::test]
async fn bounded_output_keeps_disable_responsive() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("flood-service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "output-flood",
  "command": "while true; do printf 'served-output-flood-0123456789\\n'; done",
  "tty": false,
  "restart": "never"
}
"#,
    )
    .expect("config");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable");
    wait_for_state(&paths, "output-flood", ServiceState::Running).await;

    timeout(
        Duration::from_secs(3),
        client::expect_ok(
            &paths,
            Request::Disable {
                target: Target::Name("output-flood".to_owned()),
            },
        ),
    )
    .await
    .expect("disable must not block behind output backpressure")
    .expect("disable");
}

#[tokio::test]
async fn daemon_uses_fixed_home_paths_and_rejects_duplicate_manager() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };

    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", root.path().join("wrong-config"))
        .env("XDG_RUNTIME_DIR", root.path().join("wrong-runtime"))
        .env("XDG_STATE_HOME", root.path().join("wrong-state"))
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;

    let stream = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .expect("connect old protocol client");
    let mut frame = served::protocol::framed(stream);
    served::protocol::send_json(&mut frame, &Request::Hello { version: 3 })
        .await
        .expect("send old protocol hello");
    let response = served::protocol::receive_json::<Response>(&mut frame)
        .await
        .expect("receive protocol rejection");
    assert!(matches!(
        response,
        Response::Error { message } if message.contains("unsupported protocol version 3")
    ));

    let duplicate = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .output()
        .expect("spawn duplicate manager");
    assert!(!duplicate.status.success());
    let stderr = String::from_utf8_lossy(&duplicate.stderr);
    assert!(stderr.contains("manager already running"), "{stderr}");
    assert!(!root.path().join("wrong-runtime/served.sock").exists());

    let invalid_handoff = client::request(
        &paths,
        Request::ManagerHandoff {
            executable: "relative/served".to_owned(),
        },
    )
    .await
    .expect_err("relative handoff executable must be rejected");
    assert!(invalid_handoff.to_string().contains("absolute path"));
}

#[tokio::test]
async fn manager_crash_keeps_runner_and_service_alive_for_adoption() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "survivor",
  "command": "while true; do printf 'survivor-live\\n'; sleep 0.1; done",
  "tty": false,
  "restart": "always"
}
"#,
    )
    .expect("config");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let mut first = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn first manager");
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable survivor");
    wait_for_state(&paths, "survivor", ServiceState::Running).await;
    wait_for_path(&paths.runner_socket("survivor")).await;
    let first_pid = service_pid(&paths, "survivor").await;

    first.kill().expect("kill manager");
    first.wait().expect("reap first manager");

    let second = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn replacement manager");
    let _guard = DaemonGuard(second);
    wait_for_path(&paths.socket_path()).await;
    wait_for_state(&paths, "survivor", ServiceState::Running).await;
    assert_eq!(service_pid(&paths, "survivor").await, first_pid);
    wait_for_output_tail(&paths, "survivor", "survivor-live").await;

    let session = client::attach(&paths, "survivor".to_owned())
        .await
        .expect("attach after manager recovery");
    let mut stream = session.stream;
    let output = read_until(&mut stream, b"survivor-live").await;
    assert!(
        output
            .windows(b"survivor-live".len())
            .any(|window| window == b"survivor-live")
    );
    drop(stream);

    client::expect_ok(
        &paths,
        Request::Disable {
            target: Target::Name("survivor".to_owned()),
        },
    )
    .await
    .expect("disable survivor");
}

#[tokio::test]
async fn shutdown_stops_runners_after_manager_crash() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  name: "fallback-shutdown",
  command: "sleep 60",
  tty: false,
  restart: "never",
}
"#,
    )
    .expect("config");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable fallback service");
    wait_for_state(&paths, "fallback-shutdown", ServiceState::Running).await;
    let pid = service_pid(&paths, "fallback-shutdown").await;

    daemon.kill().expect("kill manager");
    daemon.wait().expect("reap manager");
    let shutdown = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("shutdown")
        .env("HOME", &home)
        .output()
        .expect("run fallback shutdown");
    assert!(shutdown.status.success(), "shutdown failed: {shutdown:?}");
    wait_for_absent(&paths.runner_socket("fallback-shutdown")).await;
    wait_for_process_exit(pid).await;
}

#[tokio::test]
async fn manager_handoff_preserves_service_and_shutdown_stops_runners() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "handoff",
  "command": "sleep 60",
  "tty": false,
  "restart": "never"
}
"#,
    )
    .expect("config");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let first_binary = root.path().join("served-old");
    let next_binary = root.path().join("served-new");
    fs::copy(env!("CARGO_BIN_EXE_served"), &first_binary).expect("copy old manager binary");
    fs::copy(env!("CARGO_BIN_EXE_served"), &next_binary).expect("copy new manager binary");
    fs::set_permissions(&first_binary, fs::Permissions::from_mode(0o755))
        .expect("make old manager executable");
    fs::set_permissions(&next_binary, fs::Permissions::from_mode(0o755))
        .expect("make new manager executable");

    let mut daemon = Command::new(&first_binary)
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable handoff service");
    wait_for_state(&paths, "handoff", ServiceState::Running).await;
    let pid_before = service_pid(&paths, "handoff").await;
    fs::remove_file(&first_binary).expect("remove old manager path");

    let handoff = Command::new(&next_binary)
        .args(["daemon", "--handoff"])
        .env("HOME", &home)
        .output()
        .expect("run manager handoff");
    assert!(handoff.status.success(), "handoff failed: {handoff:?}");
    wait_for_state(&paths, "handoff", ServiceState::Running).await;
    assert_eq!(service_pid(&paths, "handoff").await, pid_before);

    let shutdown = Command::new(&next_binary)
        .arg("shutdown")
        .env("HOME", &home)
        .output()
        .expect("run manager shutdown");
    assert!(shutdown.status.success(), "shutdown failed: {shutdown:?}");
    for _ in 0..100 {
        if daemon.try_wait().expect("poll manager").is_some() {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert!(daemon.try_wait().expect("recheck manager").is_some());
    wait_for_absent(&paths.runner_socket("handoff")).await;
}

#[tokio::test]
async fn manager_relinquish_preserves_runner_for_a_new_supervisor() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  name: "relinquish",
  command: "sleep 60",
  tty: false,
  restart: "never",
}
"#,
    )
    .expect("config");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let mut first = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn first manager");
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable relinquish service");
    wait_for_state(&paths, "relinquish", ServiceState::Running).await;
    let service_pid = service_pid(&paths, "relinquish").await;

    let relinquish = Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["daemon", "--relinquish"])
        .env("HOME", &home)
        .output()
        .expect("request manager relinquish");
    assert!(
        relinquish.status.success(),
        "relinquish client failed: {relinquish:?}"
    );
    let status = first.wait().expect("reap relinquished manager");
    assert_eq!(status.code(), Some(SUPERVISOR_RELINQUISH_EXIT_CODE));
    assert!(paths.runner_socket("relinquish").exists());
    assert!(process_exists(service_pid));

    let replacement = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn replacement manager");
    let _guard = DaemonGuard(replacement);
    wait_for_path(&paths.socket_path()).await;
    wait_for_state(&paths, "relinquish", ServiceState::Running).await;
    assert_eq!(self::service_pid(&paths, "relinquish").await, service_pid);

    let shutdown = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("shutdown")
        .env("HOME", &home)
        .output()
        .expect("shutdown replacement manager");
    assert!(shutdown.status.success(), "shutdown failed: {shutdown:?}");
    wait_for_process_exit(service_pid).await;
}

#[tokio::test]
async fn direct_attach_supports_name_and_current_directory() {
    let root = test_root();
    let home = root.path().join("home");
    let config_home = home.join(".config");
    let runtime_dir = home.join(".local/state/served/runtime");
    let state_home = home.join(".local/state");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "direct-attach",
  "command": "while true; do stty size; printf 'attach-ready\\n'; sleep 1; done",
  "tty": true,
  "restart": "never"
}
"#,
    )
    .expect("config");
    fs::write(service_dir.join(".env.served"), "").expect("env.served");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
        state_home,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .spawn()
        .expect("spawn manager");
    let _guard = DaemonGuard(daemon);
    wait_for_path(&paths.socket_path()).await;
    client::expect_ok(
        &paths,
        Request::Enable {
            file: None,
            workdir: None,
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable");
    wait_for_state(&paths, "direct-attach", ServiceState::Running).await;

    run_direct_attach(&paths, &service_dir, Some("direct-attach"));
    wait_for_state(&paths, "direct-attach", ServiceState::Running).await;
    run_direct_attach(&paths, &service_dir, None);
    wait_for_state(&paths, "direct-attach", ServiceState::Running).await;

    client::expect_ok(
        &paths,
        Request::Disable {
            target: Target::Name("direct-attach".to_owned()),
        },
    )
    .await
    .expect("disable");
}

fn run_direct_attach(paths: &ServedPaths, directory: &Path, name: Option<&str>) {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 40,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open test pty");
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_served"));
    command.arg("attach");
    if let Some(name) = name {
        command.arg(name);
    }
    command.env("HOME", paths.config_home.parent().expect("home"));
    command.cwd(directory);
    let mut child = pair.slave.spawn_command(command).expect("spawn attach");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let mut writer = pair.master.take_writer().expect("take pty writer");
    let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
    let (output_sender, output_receiver) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    let _ = output_sender.send(output);
                    return;
                }
                Ok(count) => {
                    output.extend_from_slice(&buffer[..count]);
                    let ready = output
                        .windows(b"attach-ready".len())
                        .any(|window| window == b"attach-ready");
                    let resized = output
                        .windows(b"40 100".len())
                        .any(|window| window == b"40 100");
                    if ready && resized {
                        let _ = ready_sender.send(());
                    }
                }
            }
        }
    });
    ready_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("wait for attach session");

    writer.write_all(&[0x03]).expect("send Ctrl-C detach");
    writer.flush().expect("flush detach");
    let status = wait_for_pty_child(&mut child);
    assert!(status.success(), "attach exited unsuccessfully: {status:?}");
    reader_thread.join().expect("join pty reader");
    let output = output_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("collect attach terminal output");
    assert!(
        output
            .windows(b"\x1b[?1049h".len())
            .any(|window| window == b"\x1b[?1049h")
    );
    assert!(
        output
            .windows(b"\x1b[?1049l".len())
            .any(|window| window == b"\x1b[?1049l")
    );
}

async fn read_until<R>(stream: &mut R, needle: &[u8]) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    timeout(Duration::from_secs(3), async {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer).await.expect("read attach output");
            if count == 0 {
                return output;
            }
            output.extend_from_slice(&buffer[..count]);
            if output.windows(needle.len()).any(|window| window == needle) {
                return output;
            }
        }
    })
    .await
    .expect("attach output timeout")
}

fn wait_for_pty_child(child: &mut Box<dyn PtyChild + Send + Sync>) -> portable_pty::ExitStatus {
    for _ in 0..250 {
        if let Some(status) = child.try_wait().expect("poll attach child") {
            return status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    panic!("timed out waiting for attach child");
}

async fn history_records(paths: &ServedPaths, name: &str) -> Vec<served::protocol::HistoryRecord> {
    let response = client::request(
        paths,
        Request::HistoryList {
            target: Target::Name(name.to_owned()),
        },
    )
    .await
    .expect("history list");
    let Response::HistoryList { records, .. } = response else {
        panic!("unexpected history list response");
    };
    records
}

async fn read_history(paths: &ServedPaths, name: &str, id: &str, limit: u32) -> String {
    let mut offset = 0_u64;
    let mut content = String::new();
    loop {
        let response = client::request(
            paths,
            Request::HistoryChunk {
                target: Target::Name(name.to_owned()),
                id: id.to_owned(),
                offset,
                limit,
            },
        )
        .await
        .expect("history chunk");
        let Response::HistoryChunk {
            next_offset,
            eof,
            content: chunk,
            ..
        } = response
        else {
            panic!("unexpected history chunk response");
        };
        content.push_str(&chunk);
        if eof {
            return content;
        }
        assert!(next_offset > offset, "history reader must make progress");
        offset = next_offset;
    }
}

async fn wait_for_attach_unavailable(paths: &ServedPaths, name: &str) -> client::AttachUnavailable {
    for _ in 0..250 {
        match client::attach(paths, name.to_owned()).await {
            Ok(session) => drop(session),
            Err(error) => {
                if let Some(unavailable) = error.downcast_ref::<client::AttachUnavailable>() {
                    return unavailable.clone();
                }
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for crash-loop attach diagnostic for {name}");
}

async fn wait_for_path(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {}", path.display());
}

async fn wait_for_absent(path: &Path) {
    for _ in 0..100 {
        if !path.exists() {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {} to disappear", path.display());
}

async fn wait_for_process_exit(pid: u32) {
    for _ in 0..100 {
        if !process_exists(pid) {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for process {pid} to exit");
}

async fn wait_for_process(pid: u32) {
    for _ in 0..100 {
        if process_exists(pid) {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for process {pid}");
}

fn process_exists(pid: u32) -> bool {
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        let is_zombie = stat
            .rsplit_once(") ")
            .and_then(|(_, fields)| fields.as_bytes().first())
            == Some(&b'Z');
        if is_zombie {
            return false;
        }
    }
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    match kill(Pid::from_raw(pid), None) {
        Ok(()) | Err(Errno::EPERM) => true,
        Err(Errno::ESRCH) => false,
        Err(_) => true,
    }
}

fn test_root() -> TempDir {
    let temp_root = fs::canonicalize("/tmp").expect("canonical temp root");
    Builder::new()
        .prefix("served-")
        .tempdir_in(temp_root)
        .expect("short test tempdir")
}

async fn wait_for_state(paths: &ServedPaths, name: &str, expected: ServiceState) {
    for _ in 0..100 {
        if let Ok(Response::Services { services }) = client::request(paths, Request::List).await {
            if services
                .iter()
                .any(|service| service.name == name && same_state(&service.state, &expected))
            {
                return;
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for service state");
}

async fn wait_for_output_tail(paths: &ServedPaths, name: &str, needle: &str) {
    for _ in 0..100 {
        if let Ok(Response::Services { services }) = client::request(paths, Request::List).await {
            if services
                .iter()
                .any(|service| service.name == name && service.output_tail.contains(needle))
            {
                return;
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for output tail");
}

async fn service_pid(paths: &ServedPaths, name: &str) -> u32 {
    for _ in 0..100 {
        if let Ok(Response::Services { services }) = client::request(paths, Request::List).await {
            if let Some(pid) = services
                .iter()
                .find(|service| service.name == name)
                .and_then(|service| service.pid)
            {
                return pid;
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for service pid");
}

async fn wait_for_attach_state(paths: &ServedPaths, name: &str, expected: bool) {
    for _ in 0..100 {
        if let Ok(Response::Services { services }) = client::request(paths, Request::List).await {
            if services
                .iter()
                .any(|service| service.name == name && service.attach_active == expected)
            {
                return;
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for attach state");
}

fn same_state(left: &ServiceState, right: &ServiceState) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}

struct LifecycleHarness {
    _root: TempDir,
    home: std::path::PathBuf,
    directory: std::path::PathBuf,
    paths: ServedPaths,
    daemon: DaemonGuard,
}

impl LifecycleHarness {
    async fn new() -> Self {
        let root = test_root();
        let home = root.path().join("home");
        let directory = root.path().join("service");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir(&directory).unwrap();
        let paths = ServedPaths {
            config_home: home.join(".config"),
            runtime_dir: home.join(".local/state/served/runtime"),
            state_home: home.join(".local/state"),
        };
        let daemon = DaemonGuard(Self::spawn(&home));
        wait_for_path(&paths.socket_path()).await;
        Self {
            _root: root,
            home,
            directory,
            paths,
            daemon,
        }
    }

    fn spawn(home: &Path) -> Child {
        Command::new(env!("CARGO_BIN_EXE_served"))
            .arg("daemon")
            .env("HOME", home)
            .spawn()
            .unwrap()
    }

    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_served"))
            .args(args)
            .env("HOME", &self.home)
            .current_dir(&self.directory)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) {
        let result = self.cli(args);
        assert!(result.status.success(), "{args:?}: {result:?}");
    }

    fn config(&self, tty: bool, command: &str, restart: &str) {
        fs::write(
            self.directory.join(".served.json5"),
            serde_json::to_vec(&serde_json::json!({
                "name": "api", "command": command, "tty": tty, "restart": restart, "persist_logs": tty
            }))
            .unwrap(),
        )
        .unwrap();
    }

    async fn status(&self, name: &str) -> served::runner_protocol::RunnerStatus {
        let response = served::runner_protocol::request(
            &self.paths.runner_socket(name),
            name,
            served::runner_protocol::RunnerRequest::Status,
        )
        .await
        .unwrap();
        let served::runner_protocol::RunnerResponse::Status { status } = response else {
            panic!("status")
        };
        status
    }

    async fn assert_stopped(&self, name: &str) {
        wait_for_state(&self.paths, name, ServiceState::Stopped).await;
        let status = self.status(name).await;
        assert!(status.manually_stopped);
        assert!(status.pid.is_none());
        assert!(self.paths.runner_metadata(name).exists());
    }

    async fn shutdown(&mut self) {
        self.ok(&["shutdown"]);
        for name in ["api", "temporary"] {
            assert!(!self.paths.runner_socket(name).exists());
        }
        wait_for_absent(&self.paths.socket_path()).await;
        self.daemon.0.wait().unwrap();
        for name in ["api", "temporary"] {
            wait_for_absent(&self.paths.runner_socket(name)).await;
        }
    }
}

#[tokio::test]
async fn start_stop_preserve_registration_history_and_reload_only_when_stopped() {
    for tty in [false, true] {
        let mut h = LifecycleHarness::new().await;
        h.config(tty, "printf 'first-run\\n'; exec sleep 60", "always");
        h.ok(&["enable"]);
        wait_for_output_tail(&h.paths, "api", "first-run").await;
        let original = h.status("api").await;
        let mut attach = client::attach(&h.paths, "api".to_owned()).await.unwrap();
        read_until(&mut attach.stream, b"first-run").await;
        fs::write(h.directory.join(".served.json5"), "invalid JSON5").unwrap();
        h.ok(&["start"]); // Running start must not read invalid config or replace the PID.
        assert_eq!(h.status("api").await.pid, original.pid);
        h.ok(&["stop"]);
        h.ok(&["stop", "api"]);
        h.assert_stopped("api").await;
        wait_for_process_exit(original.pid.unwrap()).await;
        let mut tail = Vec::new();
        timeout(Duration::from_secs(3), attach.stream.read_to_end(&mut tail))
            .await
            .unwrap()
            .unwrap();
        assert!(h.paths.registry_dir().join("api").exists());
        assert!(
            read_history(&h.paths, "api", "latest", 1024)
                .await
                .contains("first-run")
        );
        assert!(!h.cli(&["start"]).status.success());
        h.assert_stopped("api").await;
        h.config(tty, "printf 'second-run\\n'; exec sleep 60", "always");
        h.ok(&["start", "api"]);
        wait_for_output_tail(&h.paths, "api", "second-run").await;
        let current = h.status("api").await;
        assert!(!current.manually_stopped);
        assert_eq!(current.runner_pid, original.runner_pid);
        assert_ne!(current.pid, original.pid);
        assert!(history_records(&h.paths, "api").await.len() >= 2);
        for _ in 0..5 {
            h.ok(&["stop"]);
            h.ok(&["start"]);
            wait_for_state(&h.paths, "api", ServiceState::Running).await;
        }
        h.ok(&["stop"]);
        h.ok(&["restart"]);
        wait_for_state(&h.paths, "api", ServiceState::Running).await;
        h.ok(&["disable"]);
        assert!(!h.cli(&["start", "api"]).status.success());
        fs::write(h.directory.join(".served.json5"), "invalid JSON5").unwrap();
        let mut args = vec!["run", "--name", "temporary", "--env", "SAVED=original"];
        if !tty {
            args.push("--no-tty");
        }
        args.extend(["--", "sh", "-c", "printf '%s\\n' \"$SAVED\"; exec sleep 60"]);
        h.ok(&args);
        wait_for_output_tail(&h.paths, "temporary", "original").await;
        let temporary = h.status("temporary").await;
        h.ok(&["stop", "temporary"]);
        h.assert_stopped("temporary").await;
        assert!(h.paths.transient_definition("temporary").exists());
        assert!(
            read_history(&h.paths, "temporary", "latest", 1024)
                .await
                .contains("original")
        );
        h.ok(&["start", "temporary"]);
        wait_for_output_tail(&h.paths, "temporary", "original").await;
        let restarted = h.status("temporary").await;
        assert_eq!(temporary.runner_pid, restarted.runner_pid);
        assert_ne!(temporary.pid, restarted.pid);
        h.shutdown().await;
    }
}

#[tokio::test]
async fn stopped_services_survive_adoption_but_only_enabled_services_return_after_shutdown() {
    let mut h = LifecycleHarness::new().await;
    h.config(false, "printf 'kept-history\\n'; exec sleep 60", "always");
    h.ok(&["enable"]);
    wait_for_output_tail(&h.paths, "api", "kept-history").await;
    h.ok(&[
        "run",
        "--name",
        "temporary",
        "--env",
        "SAVED=original",
        "--",
        "sh",
        "-c",
        "printf '%s\\n' \"$SAVED\"; exec sleep 60",
    ]);
    wait_for_output_tail(&h.paths, "temporary", "original").await;
    assert!(!h.cli(&["stop"]).status.success()); // Shared workdir requires an explicit name.
    assert!(!h.cli(&["start"]).status.success());
    for name in ["api", "temporary"] {
        h.ok(&["stop", name]);
    }
    let enabled_runner = h.status("api").await.runner_pid;
    let temporary_runner = h.status("temporary").await.runner_pid;
    // A broken edited configuration must not prevent adopting a manually stopped runner.
    fs::write(h.directory.join(".served.json5"), "invalid JSON5").unwrap();
    h.ok(&["daemon", "--handoff"]);
    for name in ["api", "temporary"] {
        h.assert_stopped(name).await;
    }
    h.daemon.0.kill().unwrap();
    h.daemon.0.wait().unwrap();
    h.daemon = DaemonGuard(LifecycleHarness::spawn(&h.home));
    for name in ["api", "temporary"] {
        h.assert_stopped(name).await;
    }
    assert_eq!(h.status("api").await.runner_pid, enabled_runner);
    assert_eq!(h.status("temporary").await.runner_pid, temporary_runner);
    assert!(
        read_history(&h.paths, "api", "latest", 1024)
            .await
            .contains("kept-history")
    );
    h.ok(&["daemon", "--relinquish"]);
    h.daemon.0.wait().unwrap();
    h.daemon = DaemonGuard(LifecycleHarness::spawn(&h.home));
    for name in ["api", "temporary"] {
        h.assert_stopped(name).await;
    }
    h.ok(&["start", "temporary"]);
    wait_for_output_tail(&h.paths, "temporary", "original").await;
    h.ok(&["stop", "temporary"]);
    h.config(false, "exec sleep 60", "always");
    h.shutdown().await;
    h.daemon = DaemonGuard(LifecycleHarness::spawn(&h.home));
    wait_for_state(&h.paths, "api", ServiceState::Running).await;
    let Response::Services { services } = client::request(&h.paths, Request::List).await.unwrap()
    else {
        panic!("list")
    };
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].name, "api");
    h.shutdown().await;
}

#[tokio::test]
async fn stop_cancels_backoff_and_start_stop_handle_quick_exits() {
    let mut h = LifecycleHarness::new().await;
    h.config(false, "exit 1", "always");
    h.ok(&["enable"]);
    wait_for_state(&h.paths, "api", ServiceState::Restarting).await;
    h.ok(&["start", "api"]);
    h.ok(&["stop", "api"]);
    h.assert_stopped("api").await;
    sleep(Duration::from_millis(600)).await;
    h.assert_stopped("api").await;
    h.config(false, "exit 0", "never");
    for _ in 0..20 {
        h.ok(&["start", "api"]);
        h.ok(&["stop", "api"]);
        h.assert_stopped("api").await;
    }
    h.ok(&["disable", "api"]);
    h.shutdown().await;
}

#[tokio::test]
async fn stopped_runner_replacement_does_not_launch_a_process() {
    let mut h = LifecycleHarness::new().await;
    h.config(false, "exec sleep 60", "always");
    h.ok(&["enable"]);
    wait_for_state(&h.paths, "api", ServiceState::Running).await;
    h.ok(&["stop", "api"]);
    let old = h.status("api").await;
    kill(
        Pid::from_raw(old.runner_pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(served::runner_protocol::RunnerResponse::Status { status }) =
                served::runner_protocol::request(
                    &h.paths.runner_socket("api"),
                    "api",
                    served::runner_protocol::RunnerRequest::Status,
                )
                .await
            {
                if status.runner_pid != old.runner_pid && status.manually_stopped {
                    assert!(status.pid.is_none());
                    break;
                }
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("manager must replace the failed runner without launching its service");
    h.assert_stopped("api").await;
    h.ok(&["start", "api"]);
    wait_for_state(&h.paths, "api", ServiceState::Running).await;
    h.shutdown().await;
}

#[tokio::test]
async fn handoff_reaps_stopped_runners_and_preserves_manual_stop() {
    let mut h = LifecycleHarness::new().await;
    h.config(false, "exec sleep 60", "always");
    h.ok(&["enable"]);
    h.ok(&[
        "run",
        "--name",
        "temporary",
        "--restart",
        "always",
        "--",
        "sleep",
        "60",
    ]);
    let mut old_runners = Vec::new();
    for name in ["api", "temporary"] {
        wait_for_state(&h.paths, name, ServiceState::Running).await;
        h.ok(&["stop", name]);
        old_runners.push(h.status(name).await.runner_pid);
    }
    h.ok(&["daemon", "--handoff"]);
    for name in ["api", "temporary"] {
        h.assert_stopped(name).await;
    }
    for old in &old_runners {
        kill(
            Pid::from_raw(*old as i32),
            nix::sys::signal::Signal::SIGKILL,
        )
        .unwrap();
    }
    for (name, old) in ["api", "temporary"].into_iter().zip(old_runners) {
        timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(served::runner_protocol::RunnerResponse::Status { status }) =
                    served::runner_protocol::request(
                        &h.paths.runner_socket(name),
                        name,
                        served::runner_protocol::RunnerRequest::Status,
                    )
                    .await
                {
                    if status.runner_pid != old && status.manually_stopped {
                        assert!(status.pid.is_none());
                        break;
                    }
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("handoff must not prevent stopped runner replacement");
        // kill(pid, 0) still succeeds for zombies: require actual reaping, not only
        // the absence of a running process reported by process_exists().
        wait_for_reaped_pid(old).await;
        h.assert_stopped(name).await;
        h.ok(&["start", name]);
        wait_for_state(&h.paths, name, ServiceState::Running).await;
    }
    // A second handoff must also reap orderly exits whose metadata was removed.
    h.ok(&["daemon", "--handoff"]);
    for name in ["api", "temporary"] {
        let runner = h.status(name).await.runner_pid;
        h.ok(&["disable", name]);
        wait_for_reaped_pid(runner).await;
    }
    h.shutdown().await;
}

async fn wait_for_reaped_pid(pid: u32) {
    timeout(Duration::from_secs(5), async {
        loop {
            if kill(Pid::from_raw(pid as i32), None) == Err(Errno::ESRCH) {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("runner must be reaped, not retained as a zombie");
}

#[cfg(feature = "tui")]
#[tokio::test]
async fn tui_menu_drives_lifecycle_and_returns_from_attach() {
    let mut harness = LifecycleHarness::new().await;
    harness.config(true, "printf 'tui-ready\\n'; exec sleep 60", "never");
    harness.ok(&["enable"]);
    wait_for_state(&harness.paths, "api", ServiceState::Running).await;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_served"));
    command.env("HOME", &harness.home);
    command.env("TERM", "xterm-256color");
    command.cwd(&harness.directory);
    struct TuiChild(Box<dyn PtyChild + Send + Sync>);
    impl Drop for TuiChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = TuiChild(pair.slave.spawn_command(command).unwrap());
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let output_thread = std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        let mut terminal = vt100::Parser::new(24, 80, 0);
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    terminal.process(&buffer[..n]);
                    if sender.send(terminal.screen().contents()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let wait_text = |needle: &str| {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut screen = String::new();
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            screen = receiver
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("TUI did not display {needle}: {screen}"));
            if screen.contains(needle) {
                break;
            }
        }
    };
    let mut send = |keys: &[u8]| {
        writer.write_all(keys).unwrap();
        writer.flush().unwrap();
    };
    wait_text("enter actions");
    send(b"\r?");
    wait_text("Help");
    send(b"qx");
    harness.assert_stopped("api").await;
    // Wait for the UI acknowledgment, not just the earlier runner state change.
    wait_text("stopped api");
    send(b"s");
    wait_for_state(&harness.paths, "api", ServiceState::Running).await;
    wait_text("started api");
    // Cancel is the default, so Enter after d must not remove the service.
    send(b"d\ra");
    wait_for_attach_state(&harness.paths, "api", true).await;
    send(&[3]);
    wait_for_attach_state(&harness.paths, "api", false).await;
    send(b"h");
    wait_text("/ history");
    send(b"\r");
    wait_text("tui-ready");
    // Close content and history, confirm disable from the original action menu.
    send(b"qqdj\r");
    wait_text("No services.");
    let Response::Services { services } = client::request(&harness.paths, Request::List)
        .await
        .unwrap()
    else {
        panic!("list response");
    };
    assert!(services.is_empty());
    send(b"q");
    assert!(wait_for_pty_child(&mut child.0).success());
    output_thread.join().unwrap();
    harness.shutdown().await;
}

#[tokio::test]
async fn stream_attach_keeps_output_after_stdin_eof_and_preserves_control_bytes() {
    use std::process::Stdio;
    let mut h = LifecycleHarness::new().await;
    h.config(true, "stty raw -echo; printf 'ready'; dd bs=1 count=3 2>/dev/null; sleep 0.2; printf 'delayed'; exit 7", "never");
    h.ok(&["enable"]);
    wait_for_output_tail(&h.paths, "api", "ready").await;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_served"))
        .args(["attach", "api", "--stream"])
        .env("HOME", &h.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"a\x03b")
        .await
        .unwrap();
    let result = timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(
        result.stdout.windows(3).any(|bytes| bytes == b"a\x03b"),
        "{result:?}"
    );
    assert!(result.stdout.ends_with(b"delayed"), "{result:?}");
    assert!(
        !result.stdout.contains(&0x1b),
        "attach added terminal escapes"
    );
    assert!(result.stderr.is_empty(), "{result:?}");
    h.shutdown().await;
}

#[tokio::test]
async fn stream_attach_cancels_blocked_input_and_output_without_stopping_service() {
    use nix::sys::signal::Signal;
    use std::process::Stdio;
    let mut h = LifecycleHarness::new().await;
    h.config(
        false,
        "while true; do printf 'data-data-data-data\\n'; done",
        "never",
    );
    h.ok(&["enable"]);
    wait_for_state(&h.paths, "api", ServiceState::Running).await;
    for (args, signal, expected) in [
        (vec!["attach", "api"], Some(Signal::SIGTERM), 143),
        (
            vec!["attach", "api", "--no-stdin"],
            Some(Signal::SIGINT),
            130,
        ),
        (vec!["attach", "api", "--stream"], None, 0),
    ] {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_served"))
            .args(args)
            .env("HOME", &h.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        wait_for_attach_state(&h.paths, "api", true).await;
        sleep(Duration::from_millis(200)).await;
        if let Some(signal) = signal {
            kill(Pid::from_raw(child.id().unwrap() as i32), signal).unwrap();
        } else {
            drop(child.stdout.take());
        }
        let status = timeout(Duration::from_secs(3), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.code(), Some(expected));
        wait_for_attach_state(&h.paths, "api", false).await;
        wait_for_state(&h.paths, "api", ServiceState::Running).await;
    }
    h.shutdown().await;
}
