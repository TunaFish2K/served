use std::{
    fs,
    io::{Read, Write},
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
    let config_home = root.path().join("config");
    let runtime_dir = root.path().join("runtime");
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
    fs::write(service_dir.join(".env"), "SMOKE_PORT=8080\n").expect("env");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("XDG_CONFIG_HOME", &paths.config_home)
        .env("XDG_RUNTIME_DIR", &paths.runtime_dir)
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

    fs::write(service_dir.join(".env"), "SMOKE_PORT=9090\n").expect("new env");
    client::expect_ok(
        &paths,
        Request::Restart {
            target: Target::Name("smoke".to_owned()),
        },
    )
    .await
    .expect("restart with changed env");
    wait_for_state(&paths, "smoke", ServiceState::Stopped).await;

    fs::write(service_dir.join(".env"), "SMOKE_PORT=8080\n").expect("restored env");
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
async fn pty_service_accepts_one_attach_session() {
    let root = tempdir().expect("tempdir");
    let config_home = root.path().join("config");
    let runtime_dir = root.path().join("runtime");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "pty-smoke",
  "command": "read -r line; printf 'reply:%s\\n' \"$line\"; sleep 60",
  "tty": true,
  "restart": "never"
}
"#,
    )
    .expect("config");
    fs::write(service_dir.join(".env"), "").expect("env");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("XDG_CONFIG_HOME", &paths.config_home)
        .env("XDG_RUNTIME_DIR", &paths.runtime_dir)
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
    let mut stream = client::attach(&paths, "pty-smoke".to_owned())
        .await
        .expect("attach");
    stream
        .write_all(b"hello\n")
        .await
        .expect("write attach input");
    let second_attach = client::attach(&paths, "pty-smoke".to_owned()).await;
    assert!(second_attach.is_err(), "second attach must be rejected");
    let mut output = Vec::new();
    let mut buffer = [0_u8; 1024];
    timeout(Duration::from_secs(2), async {
        loop {
            let count = stream.read(&mut buffer).await.expect("read attach output");
            if count == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..count]);
            if String::from_utf8_lossy(&output).contains("reply:hello") {
                break;
            }
        }
    })
    .await
    .expect("attach output timeout");
    assert!(String::from_utf8_lossy(&output).contains("reply:hello"));
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
    let config_home = root.path().join("config");
    let runtime_dir = root.path().join("runtime");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "pipe-attach",
  "command": "while true; do printf 'pipe-ready\\n'; sleep 1; done",
  "tty": false,
  "restart": "never"
}
"#,
    )
    .expect("config");
    fs::write(service_dir.join(".env"), "").expect("env");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("XDG_CONFIG_HOME", &paths.config_home)
        .env("XDG_RUNTIME_DIR", &paths.runtime_dir)
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

    let mut first = client::attach(&paths, "pipe-attach".to_owned())
        .await
        .expect("first read-only attach");
    let mut second = client::attach(&paths, "pipe-attach".to_owned())
        .await
        .expect("second read-only attach");
    first
        .write_all(b"ignored input\n")
        .await
        .expect("write ignored input");
    second
        .write_all(b"ignored input\n")
        .await
        .expect("write ignored input");

    let first_output = read_until(&mut first, b"pipe-ready").await;
    let second_output = read_until(&mut second, b"pipe-ready").await;
    assert!(
        first_output
            .windows(b"pipe-ready".len())
            .any(|window| window == b"pipe-ready")
    );
    assert!(
        second_output
            .windows(b"pipe-ready".len())
            .any(|window| window == b"pipe-ready")
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
async fn direct_attach_supports_name_and_current_directory() {
    let root = tempdir().expect("tempdir");
    let config_home = root.path().join("config");
    let runtime_dir = root.path().join("runtime");
    let service_dir = root.path().join("service");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&service_dir).expect("service");
    fs::write(
        service_dir.join(".served.json"),
        r#"{
  "name": "direct-attach",
  "command": "while true; do printf 'attach-ready\\n'; sleep 1; done",
  "tty": true,
  "restart": "never"
}
"#,
    )
    .expect("config");
    fs::write(service_dir.join(".env"), "").expect("env");

    let paths = ServedPaths {
        config_home,
        runtime_dir,
    };
    let daemon = Command::new(env!("CARGO_BIN_EXE_served"))
        .arg("daemon")
        .env("XDG_CONFIG_HOME", &paths.config_home)
        .env("XDG_RUNTIME_DIR", &paths.runtime_dir)
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
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open test pty");
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_served"));
    command.arg("attach");
    if let Some(name) = name {
        command.arg(name);
    }
    command.env("XDG_CONFIG_HOME", &paths.config_home);
    command.env("XDG_RUNTIME_DIR", &paths.runtime_dir);
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
                    if output
                        .windows(b"attach-ready".len())
                        .any(|window| window == b"attach-ready")
                    {
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
