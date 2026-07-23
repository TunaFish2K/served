use std::{
    fs,
    path::Path,
    process::{Child, Command},
    time::Duration,
};

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

fn same_state(left: &ServiceState, right: &ServiceState) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}
