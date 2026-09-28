use std::process::Command;

#[test]
fn window_creates_presents_and_tears_down_cleanly() {
    // The log is append-only and shared with every other run, so the assertions
    // below have to start from an empty file.
    let _ = std::fs::remove_file(log_path());

    let out = Command::new(env!("CARGO_BIN_EXE_glassine"))
        .args(["--smoke-ms", "1500"])
        .arg("--config")
        .arg(missing_config_path())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let log = std::fs::read_to_string(log_path()).unwrap_or_default();
    assert!(log.contains("window created"), "log: {log}");
    assert!(log.contains("presented frame"), "log: {log}");
    assert!(!log.contains("present failed"), "log: {log}");
    assert!(!log.contains("ERROR"), "log: {log}");
}

fn missing_config_path() -> std::path::PathBuf {
    std::env::temp_dir().join("glassine-plan-smoke/does-not-exist.toml")
}

fn log_path() -> std::path::PathBuf {
    // %LOCALAPPDATA%\glassine\logs\glassine.log
    let base = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA");
    std::path::PathBuf::from(base).join("glassine").join("logs").join("glassine.log")
}
