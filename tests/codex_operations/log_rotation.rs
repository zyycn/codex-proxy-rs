use std::{fs, io::Write};

use codex_proxy_rs::codex::logs::rotation::{build_file_appender, RotationConfig};

#[test]
fn rolling_appender_should_write_daily_codex_log_file() {
    let dir = tempfile::tempdir().unwrap();
    let config = RotationConfig::new(dir.path(), 14);
    let mut appender = build_file_appender(&config).unwrap();

    appender.write_all(b"hello\n").unwrap();
    appender.flush().unwrap();

    let names = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert!(
        names
            .iter()
            .any(|name| name.starts_with("codex-proxy-rs.") && name.ends_with(".log")),
        "expected a daily codex log file, found {names:?}"
    );
}

#[test]
fn rotation_config_should_not_accept_size_limit() {
    let config = RotationConfig::new("logs", 14);

    assert_eq!(config.retention_days, 14);
}
