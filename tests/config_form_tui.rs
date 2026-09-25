#![cfg(feature = "tui")]

use portable_pty::{Child, CommandBuilder, PtySize, native_pty_system};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

// macOS TMPDIR can make the per-service Unix socket exceed sockaddr_un's limit.
fn test_directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("served-form-")
        .tempdir_in("/tmp")
        .unwrap()
}

struct Session {
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    screens: mpsc::Receiver<String>,
    output: mpsc::Receiver<Vec<u8>>,
    enhanced: bool,
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Session {
    fn start(directory: &Path, args: &[&str]) -> Self {
        Self::start_mode(directory, args, false)
    }
    fn start_mode(directory: &Path, args: &[&str], enhanced: bool) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_served"));
        command.args(args);
        command.cwd(directory);
        command.env("HOME", directory);
        command.env("TERM", "xterm-256color");
        // The full build defaults to the configuration form, even when EDITOR is set.
        command.env("EDITOR", "exit 99");
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let (screen_sender, screens) = mpsc::channel();
        let (output_sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut parser = vt100::Parser::new(24, 80, 0);
            let mut bytes = [0; 4096];
            let mut output = Vec::new();
            let mut queried = false;
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        output.extend_from_slice(&bytes[..n]);
                        if !queried && output.windows(4).any(|w| w == b"\x1b[?u") {
                            queried = true;
                            let _ = screen_sender.send("keyboard-query".into());
                        }
                        parser.process(&bytes[..n]);
                        let _ = screen_sender.send(parser.screen().contents());
                    }
                }
            }
            let _ = output_sender.send(output);
        });
        let mut session = Self {
            child,
            writer,
            screens,
            output,
            enhanced,
        };
        if !args.is_empty() {
            session.wait("keyboard-query");
            session.send(if enhanced {
                b"\x1b[?0u\x1b[?1;2c"
            } else {
                b"\x1b[?1;2c"
            });
        }
        session
    }
    fn send(&mut self, input: &[u8]) {
        self.writer.write_all(input).unwrap();
        self.writer.flush().unwrap();
    }
    fn paste(&mut self, input: &str) {
        self.send(format!("\x1b[200~{input}\x1b[201~").as_bytes());
    }
    fn wait(&self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last = String::new();
        loop {
            last = self
                .screens
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| {
                    panic!("editor did not display {text:?}; last screen:\n{last}")
                });
            if last.contains(text) {
                return;
            }
        }
    }
    fn overview(&mut self, modified: bool) {
        let title = if modified {
            "served / edit · modified"
        } else {
            "served / edit"
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut dismissed = false;
        loop {
            let screen = self
                .screens
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("overview not displayed");
            if !dismissed
                && screen.contains("Error / edit")
                && screen.contains("Configuration saved.")
            {
                assert!(screen.contains("Cannot check service state"));
                self.send(b"\x1b");
                dismissed = true;
            }
            if screen.lines().any(|line| line.trim() == title) && screen.contains("Basic") {
                return;
            }
        }
    }
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status:?}");
                break;
            }
            assert!(Instant::now() < deadline, "editor failed to exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        let bytes = self.output.recv_timeout(Duration::from_secs(5)).unwrap();
        if self.enhanced {
            for sequence in [b"\x1b[>1u".as_slice(), b"\x1b[<1u".as_slice()] {
                assert!(
                    bytes.windows(sequence.len()).any(|w| w == sequence),
                    "keyboard protocol was not enabled/restored"
                );
            }
        }
        for escape in [b"\x1b[?2004l".as_slice(), b"\x1b[?1049l".as_slice()] {
            assert!(
                bytes.windows(escape.len()).any(|window| window == escape),
                "terminal mode was not restored"
            );
        }
    }
}

#[test]
fn form_saves_fields_and_guards_external_changes() {
    let directory = test_directory();
    let path = directory.path().join("custom.json5");
    fs::write(&path, "// keep comment\n{name:'api', command:'true'}\n").unwrap();
    let mut session = Session::start(directory.path(), &["edit", "-f", "custom.json5"]);
    session.wait("Working directory");
    session.send(b"\x1b[B\r");
    session.wait("served / edit · Command");
    session.send(b"\x7f\x7f\x7f\x7f\r\x13");
    session.wait("Command must not be empty");
    session.send(b"\r");
    session.paste("echo 中文\necho done");
    session.send(b"\r\x13");
    session.overview(false);
    let saved = fs::read_to_string(&path).unwrap();
    assert!(saved.starts_with("// keep comment\n{name:'api', command:"));
    let value: serde_json::Value = json5::from_str(&saved).unwrap();
    assert_eq!(value["command"], "echo 中文\necho done");
    assert!(!directory.path().join(".config/served").exists());
    session.send(b"\r");
    session.paste(" extra");
    session.send(b"\x11");
    session.wait("Save changes before closing?");
    session.send(b"\r");
    session.wait("served / edit · Command");
    let external = "{name:'api', command:'echo external'}\n";
    fs::write(&path, external).unwrap();
    session.send(b"\r\x13");
    session.wait("File changed outside");
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
    session.send(b"\x1b");
    session.overview(true);
    session.send(b"\x12");
    session.wait("Discard changes and reload?");
    session.send(b"\x1b[B\r");
    session.overview(false);
    session.send(b"\x11");
    session.finish();
}

#[test]
fn discard_and_explicit_external_editor() {
    let directory = test_directory();
    let mut session = Session::start(directory.path(), &["edit"]);
    session.wait("Working directory");
    let path = directory.path().join(".served.json5");
    let original = fs::read_to_string(&path).unwrap();
    session.send(b"\r");
    session.paste("-changed");
    session.send(b"\x03");
    session.wait("Save changes before closing?");
    session.send(b"\x1b[B\x1b[B\r");
    session.finish();
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_served"))
        .current_dir(directory.path())
        .args(["edit", "--editor", "printf '%s\\n' external-editor"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "external editor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("external-editor"));
    assert!(!output.stdout.contains(&0x1b));
}

#[test]
fn first_time_enter_then_escape_preserves_multiline_draft_in_both_protocols() {
    for enhanced in [false, true] {
        let directory = test_directory();
        let path = directory.path().join(".served.json5");
        fs::write(&path, "{name:'api',command:'true'}").unwrap();
        let mut session = Session::start_mode(directory.path(), &["edit"], enhanced);
        session.wait("Working directory");
        session.send(b"\x1b[B\r");
        session.wait("served / edit · Command");
        session.send(if enhanced { b"\x1b[13;2u" } else { b"\x0a" });
        session.paste("echo second");
        session.send(b"\r");
        session.overview(true);
        session.send(b"\x1b");
        session.wait("Save changes before closing?");
        session.send(b"\r");
        session.overview(true);
        session.send(b"\x13");
        session.overview(false);
        let saved: serde_json::Value = json5::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(saved["command"], "true\necho second");
        session.send(b"\x11");
        session.finish();
    }
}

#[test]
fn environment_name_and_value_share_a_form_and_save_only_from_overview() {
    let directory = test_directory();
    let path = directory.path().join(".served.json5");
    let original = "{name:'api',command:'true'}";
    fs::write(&path, original).unwrap();
    let mut session = Session::start(directory.path(), &["edit"]);
    session.wait("Working directory");
    session.send(b"\x1b[D");
    session.wait("No variables");
    session.send(b"a");
    session.wait("Value");
    session.paste("APPLICATION_CONFIG");
    session.wait("APPLICATION_CONFIG");
    let value = "prefix  中\te\u{301} --option=value \n".repeat(100);
    // One burst crosses the event reader's 1 KiB boundary after an arrow key.
    session.send(format!("\x1b[B\x1b[200~{value}\x1b[201~").as_bytes());
    session.wait("┃");
    session.send(b"\x13");
    session.send(b"\x1b[1;5H"); // Beginning of Value; Up crosses to Name.
    session.send(b"\x1b[A\t\x1b[H");
    session.paste("NEW_");
    session.wait("NEW_APPLICATION_CONFIG");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    session.send(b"\x1b");
    session.overview(true);
    session.send(b"\x13");
    session.overview(false);
    let saved: serde_json::Value = json5::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(saved["env"]["NEW_APPLICATION_CONFIG"], value);
    session.send(b"\x11");
    session.finish();
}

#[test]
fn delete_button_confirms_and_keeps_file_unchanged_until_save() {
    let directory = test_directory();
    let path = directory.path().join(".served.json5");
    let original = "{name:'api',command:'true',env:{TOKEN:'secret'}}";
    fs::write(&path, original).unwrap();
    let mut session = Session::start(directory.path(), &["edit"]);
    session.wait("Working directory");
    session.send(b"\x1b[D\r");
    session.wait("[ Delete ]");
    session.paste("renamed_");
    session.send(b"\x1b[B\x1b[1;5F\x1b[B\r");
    session.wait("Delete variable TOKEN?");
    session.send(b"\r");
    session.wait("renamed_TOKEN");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    session.send(b"\r");
    session.wait("Delete variable TOKEN?");
    session.send(b"\x1b[B\r");
    session.wait("No variables");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    session.send(b"\x13");
    session.overview(false);
    let saved: serde_json::Value = json5::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert!(saved["env"].as_object().unwrap().is_empty());
    session.send(b"\x11");
    session.finish();
}

struct Manager {
    home: std::path::PathBuf,
    child: std::process::Child,
}
impl Manager {
    fn start(home: &Path) -> Self {
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_served"))
            .arg("daemon")
            .env("HOME", home)
            .current_dir(home)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let manager = Self {
            home: home.into(),
            child,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !manager.command(&["list"]).status.success() {
            assert!(Instant::now() < deadline, "manager startup timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
        manager
    }
    fn command(&self, args: &[&str]) -> std::process::Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_served"))
            .args(args)
            .env("HOME", &self.home)
            .current_dir(&self.home)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) {
        let out = self.command(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn services(&self) -> Vec<served::protocol::ServiceInfo> {
        let paths = served::paths::ServedPaths::from_home(&self.home);
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(served::client::request(
                &paths,
                served::protocol::Request::List,
            ))
            .unwrap();
        match response {
            served::protocol::Response::Services { services } => services,
            _ => panic!("list response"),
        }
    }
    fn pid(&self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = self.services().first().and_then(|s| s.pid) {
                return pid;
            }
            assert!(Instant::now() < deadline, "service did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        let _ = self.command(&["shutdown"]);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn edit_command_and_save(session: &mut Session) {
    session.send(b"\r\x1b[F");
    session.paste(" ");
    session.send(b"\r\x13");
}

#[test]
fn saved_running_config_can_decline_restart_restart_or_handle_a_stopped_target() {
    let dir = test_directory();
    let file = dir.path().join("custom.json5");
    fs::write(&file, "{name:'api',command:'exec sleep 300'}").unwrap();
    std::os::unix::fs::symlink(&file, dir.path().join("alias.json5")).unwrap();
    let manager = Manager::start(dir.path());
    manager.ok(&["enable", "-f", "custom.json5"]);
    let pid = manager.pid();
    let mut session = Session::start(dir.path(), &["edit", "-f", "alias.json5"]);
    session.wait("Working directory");
    session.send(b"\x1b[B");
    edit_command_and_save(&mut session);
    session.wait("Restart api to apply changes?");
    session.send(b"\r");
    session.overview(false);
    assert_eq!(manager.pid(), pid);
    edit_command_and_save(&mut session);
    session.wait("Restart api to apply changes?");
    session.send(b"\x1b[B\r");
    session.overview(false);
    assert_ne!(manager.pid(), pid);
    edit_command_and_save(&mut session);
    session.wait("Restart api to apply changes?");
    manager.ok(&["stop", "api"]);
    session.send(b"\x1b[B\r");
    session.wait("no longer running");
    session.send(b"\x1b");
    session.overview(false);
    assert!(manager.services()[0].pid.is_none());
    edit_command_and_save(&mut session);
    session.overview(false); // Stopped service: no restart prompt.
    session.send(b"\x13");
    session.overview(false); // Unchanged save: no state check or prompt.
    session.send(b"\x11");
    session.finish();
}

#[test]
fn actions_edit_uses_registered_file_and_save_exit_returns_to_actions() {
    let dir = test_directory();
    fs::create_dir(dir.path().join("configs")).unwrap();
    let file = dir.path().join("configs/service.json5");
    fs::write(&file, "{name:'api',command:'exec sleep 300'}").unwrap();
    let manager = Manager::start(dir.path());
    manager.ok(&["enable", "-f", "configs/service.json5", "--workdir", "."]);
    let pid = manager.pid();
    let mut session = Session::start(dir.path(), &[]);
    session.wait("api");
    session.send(b"\r");
    session.wait("api / actions");
    session.send(b"e");
    session.wait("keyboard-query");
    session.send(b"\x1b[?1;2c");
    session.wait("Working directory");
    session.send(b"\x1b[B\r\x1b[F");
    session.paste(" ");
    session.send(b"\r\x1b");
    session.wait("Save changes before closing?");
    session.send(b"\x1b[B\r");
    session.wait("Restart api to apply changes?");
    session.send(b"\r");
    session.wait("api / actions");
    assert_eq!(manager.pid(), pid);
    assert!(fs::read_to_string(file).unwrap().contains("sleep 300 "));
    assert!(!dir.path().join(".served.json5").exists());
    session.send(b"\x1b");
    session.wait("served · 1 service");
    session.send(b"q");
    session.finish();
}

#[test]
fn renamed_running_service_is_saved_without_automatic_reregistration() {
    let dir = test_directory();
    let file = dir.path().join(".served.json5");
    fs::write(&file, "{name:'api',command:'exec sleep 300'}").unwrap();
    let manager = Manager::start(dir.path());
    manager.ok(&["enable"]);
    let pid = manager.pid();
    let mut session = Session::start(dir.path(), &["edit"]);
    session.wait("Working directory");
    session.send(b"\r");
    session.send(b"\x1b[H");
    session.paste("new_");
    session.send(b"\r\x13");
    session.wait("service was renamed");
    session.send(b"\x1b");
    session.overview(false);
    assert!(fs::read_to_string(file).unwrap().contains("new_api"));
    assert_eq!(manager.pid(), pid);
    assert_eq!(manager.services()[0].name, "api");
    session.send(b"\x11");
    session.finish();
}

#[test]
fn restart_failure_keeps_saved_file_and_running_process() {
    let dir = test_directory();
    let file = dir.path().join(".served.json5");
    fs::write(&file, "{name:'api',command:'exec sleep 300'}").unwrap();
    let manager = Manager::start(dir.path());
    manager.ok(&["enable"]);
    let pid = manager.pid();
    let mut session = Session::start(dir.path(), &["edit"]);
    session.wait("Working directory");
    session.send(b"\x1b[B\x1b[B\r");
    session.paste("missing-directory");
    session.send(b"\r\x13");
    session.wait("Restart api to apply changes?");
    session.send(b"\x1b[B\r");
    session.wait("Restart failed");
    assert!(
        fs::read_to_string(file)
            .unwrap()
            .contains("missing-directory")
    );
    assert_eq!(manager.pid(), pid);
    session.send(b"\x1b");
    session.overview(false);
    session.send(b"\x11");
    session.finish();
}
