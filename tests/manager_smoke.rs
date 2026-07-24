use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command},
    time::Duration,
};

use portable_pty::{Child as PtyChild, CommandBuilder, PtySize, native_pty_system};
use served::{
    client,
    paths::ServedPaths,
    protocol::{Request, Response, ServiceState, Target},
};
use tempfile::tempdir;
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
    let root = tempdir().expect("tempdir");
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
    let root = tempdir().expect("tempdir");
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
}

#[tokio::test]
async fn pty_service_accepts_one_attach_session() {
    let root = tempdir().expect("tempdir");
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
    let root = tempdir().expect("tempdir");
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
async fn daemon_uses_fixed_home_paths_and_rejects_duplicate_manager() {
    let root = tempdir().expect("tempdir");
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

    let duplicate = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("HOME", &home)
        .output()
        .expect("spawn duplicate manager");
    assert!(!duplicate.status.success());
    let stderr = String::from_utf8_lossy(&duplicate.stderr);
    assert!(stderr.contains("manager already running"), "{stderr}");
    assert!(!root.path().join("wrong-runtime/served.sock").exists());
}

#[tokio::test]
async fn direct_attach_supports_name_and_current_directory() {
    let root = tempdir().expect("tempdir");
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

async fn read_until(stream: &mut tokio::net::UnixStream, needle: &[u8]) -> Vec<u8> {
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

async fn wait_for_path(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {}", path.display());
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
