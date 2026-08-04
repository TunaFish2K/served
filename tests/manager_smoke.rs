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
    protocol::{Request, Response, ServiceState, Target},
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
        service_dir.join(".served.json"),
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
            directory: service_dir.display().to_string(),
        },
    )
    .await
    .expect("enable");
    assert!(matches!(response, Response::Ok));
    wait_for_state(&paths, "smoke", ServiceState::Running).await;

    let original_config = fs::read(service_dir.join(".served.json")).expect("read config");
    fs::write(service_dir.join(".served.json"), "{ invalid json\n").expect("invalid config");
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
    fs::write(service_dir.join(".served.json"), original_config).expect("restore config");

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
  "command": "printf 'memory-output\\n'; sleep 0.2",
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
    assert!(!paths.logs_dir().join("memory").exists());
    let memory_output = read_history(&paths, "memory", &memory_archive.id, 4).await;
    assert!(memory_output.contains("memory-output"));

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
    assert!(!paths.runner_socket("handoff").exists());
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
    Builder::new()
        .prefix("served-")
        .tempdir_in("/tmp")
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
