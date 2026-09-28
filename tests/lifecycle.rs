//! The lifecycle contract: one instance per machine, the port conflict, the
//! quit path, and the position override (spec 10.1, 11, 12).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

mod common;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_glassine"))
}

/// A directory that stands in for `%LOCALAPPDATA%`, so a test never touches the
/// user's window position or log.
fn app_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glassine-lifecycle-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn config_in(dir: &Path, port: u16) -> PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(&path, format!("[server]\nport = {port}\n")).unwrap();
    path
}

fn spawn(config: &Path, local: &Path) -> Child {
    bin()
        .arg("--config")
        .arg(config)
        .env("LOCALAPPDATA", local)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn run(config: &Path, local: &Path, extra: &[&str]) -> std::process::Output {
    let mut command = bin();
    command.arg("--config").arg(config).env("LOCALAPPDATA", local);
    command.args(extra);
    command.output().unwrap()
}

fn wait_for_port(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("port {port} never opened");
}

fn wait_for_exit(child: &mut Child, what: &str, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    panic!("{what}: still running after {timeout:?}");
}

#[test]
fn a_second_instance_exits_with_code_1() {
    let _turn = common::take_turn();
    let dir = app_dir("instance");
    let config = config_in(&dir, 47331);

    let mut first = spawn(&config, &dir);
    wait_for_port(47331);

    // The override hands the second instance a port nobody holds, so only the
    // single-instance rule can explain code 1.
    let second = run(&config, &dir, &["--port", "47334"]);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert_eq!(second.status.code(), Some(1), "stderr was {stderr:?}");
    assert!(stderr.contains("已有实例在运行"), "stderr was {stderr:?}");

    let _ = first.kill();
    let _ = first.wait();
}

#[test]
fn a_busy_port_exits_with_code_3() {
    let _turn = common::take_turn();
    let held = TcpListener::bind(("127.0.0.1", 47332)).unwrap();
    let dir = app_dir("port");
    let config = config_in(&dir, 47332);

    let out = run(&config, &dir, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The port number has to be in the message: it is the only clue about who
    // holds it.
    assert_eq!(out.status.code(), Some(3), "stderr was {stderr:?}");
    assert!(stderr.contains("47332"), "stderr was {stderr:?}");
    drop(held);
}

#[test]
fn quit_responds_before_exiting() {
    let _turn = common::take_turn();
    let dir = app_dir("quit");
    let config = config_in(&dir, 47333);

    let mut child = spawn(&config, &dir);
    wait_for_port(47333);

    let mut stream = TcpStream::connect(("127.0.0.1", 47333)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.write_all(b"POST /quit HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 200"), "got {response:?}");
    assert!(response.contains(r#""ok":true"#), "got {response:?}");

    let status = wait_for_exit(&mut child, "after /quit", Duration::from_secs(10));
    assert_eq!(status.code(), Some(0));
}

#[test]
fn reset_window_position_removes_the_override() {
    let _turn = common::take_turn();
    let dir = app_dir("reset");
    let state = dir.join("glassine").join("window_state.json");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(&state, r#"{"x": 12, "y": 34}"#).unwrap();
    let config = config_in(&dir, 47335);

    let out = run(&config, &dir, &["--reset-window-position"]);
    assert_eq!(out.status.code(), Some(0), "stderr was {:?}", String::from_utf8_lossy(&out.stderr));
    assert!(!state.exists(), "override still present at {state:?}");

    // Nothing left to remove is not a failure: the flag is run twice by people
    // who are not sure whether the first attempt took.
    let again = run(&config, &dir, &["--reset-window-position"]);
    assert_eq!(again.status.code(), Some(0));
}

#[test]
fn the_override_outranks_the_configured_anchor() {
    let _turn = common::take_turn();
    let dir = app_dir("override");
    let state = dir.join("glassine").join("window_state.json");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(&state, r#"{"x": 12, "y": 34}"#).unwrap();
    let config = config_in(&dir, 47336);

    // `--check-config` resolves the same rectangle the window would use, and
    // reports which authority placed it.
    let out = run(&config, &dir, &["--check-config"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("position_source=override"), "stdout was {stdout:?}");
    assert!(stdout.contains("rect=12,34,"), "stdout was {stdout:?}");

    let cleared = run(&config, &dir, &["--reset-window-position"]);
    assert_eq!(cleared.status.code(), Some(0));
    let out = run(&config, &dir, &["--check-config"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("position_source=config"), "stdout was {stdout:?}");
}
