use std::process::Command;

#[test]
fn check_config_reports_resolved_rect_without_a_window() {
    let dir = std::env::temp_dir().join("glassine-plan-check-config");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[window]\nanchor = \"top-left\"\noffset = [0, 0]\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_glassine"))
        .arg("--check-config")
        .arg("--config")
        .arg(&path)
        .output()
        .unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("position_source=config"), "{stdout}");
    assert!(stdout.contains("port=17321"), "{stdout}");
    assert!(stdout.contains("rect=0,0,640,220"), "{stdout}");
}

#[test]
fn invalid_config_exits_with_code_2_and_names_the_field() {
    let dir = std::env::temp_dir().join("glassine-plan-check-config-bad");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[window]\nopacity = 500\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_glassine"))
        .arg("--check-config")
        .arg("--config")
        .arg(&path)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("window.opacity"), "{stderr}");
}

#[test]
fn environment_overrides_the_config_file() {
    let dir = std::env::temp_dir().join("glassine-plan-check-config-env");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[server]\nport = 17321\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_glassine"))
        .arg("--check-config")
        .arg("--config")
        .arg(&path)
        .env("GLASSINE_PORT", "49999")
        .output()
        .unwrap();

    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("port=49999"));
}
