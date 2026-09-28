use std::process::Command;

mod common;

#[test]
fn window_creates_presents_and_tears_down_cleanly() {
    let _turn = common::take_turn();

    // A log of this test's own: the assertions below read the whole file, so
    // nothing else may add a line to it between the run and the read.
    let local = std::env::temp_dir().join("glassine-plan-smoke/local-app-data");
    let _ = std::fs::remove_dir_all(&local);
    std::fs::create_dir_all(&local).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_glassine"))
        .args(["--smoke-ms", "1500"])
        .arg("--config")
        .arg(missing_config_path())
        .env("LOCALAPPDATA", &local)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let log = std::fs::read_to_string(log_path(&local)).unwrap_or_default();
    assert!(log.contains("window created"), "log: {log}");
    assert!(log.contains("presented frame"), "log: {log}");
    assert!(!log.contains("present failed"), "log: {log}");
    assert!(!log.contains("ERROR"), "log: {log}");
}

fn missing_config_path() -> std::path::PathBuf {
    std::env::temp_dir().join("glassine-plan-smoke/does-not-exist.toml")
}

#[test]
fn a_moved_window_stays_where_the_move_left_it() {
    let _turn = common::take_turn();

    let local = std::env::temp_dir().join("glassine-plan-smoke/move-local-app-data");
    let _ = std::fs::remove_dir_all(&local);
    std::fs::create_dir_all(&local).unwrap();

    // The flag moves the window as a drag does and checks, after the delay, that
    // the window's own position still matches what `present` draws at.
    let out = Command::new(env!("CARGO_BIN_EXE_glassine"))
        .args(["--smoke-move-ms", "1200"])
        .arg("--config")
        .arg(missing_config_path())
        .env("LOCALAPPDATA", &local)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    let log = std::fs::read_to_string(log_path(&local)).unwrap_or_default();
    assert!(log.contains("move mode entered"), "log: {log}");
    assert!(log.contains("presented frame"), "log: {log}");
    assert!(!log.contains("smoke:"), "log: {log}");
}

/// `%LOCALAPPDATA%\glassine\logs\glassine.log`, under this test's own directory.
fn log_path(local: &std::path::Path) -> std::path::PathBuf {
    local.join("glassine").join("logs").join("glassine.log")
}
