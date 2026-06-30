use std::{fs, io::Write};

use chrono::Utc;
use codex_proxy_rs::infra::{
    logging::{build_file_appender, RotationConfig},
    time::china_date,
};

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

    let expected = format!("codex-proxy-rs.{}.log", china_date(&Utc::now()));
    assert!(
        names.iter().any(|name| name == &expected),
        "expected China-date codex log file {expected}, found {names:?}"
    );
}

#[test]
fn rotation_config_should_not_accept_size_limit() {
    let config = RotationConfig::new("logs", 14);

    assert_eq!(config.retention_days, 14);
}

#[test]
fn rolling_appender_should_remove_managed_logs_outside_retention() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(managed_log_name("2000-01-01")), b"old").unwrap();
    fs::write(dir.path().join(managed_log_name("2001-01-01")), b"recent").unwrap();
    fs::write(dir.path().join("other.log"), b"unmanaged").unwrap();
    let config = RotationConfig::new(dir.path(), 2);

    let _appender = build_file_appender(&config).unwrap();

    assert!(!dir.path().join(managed_log_name("2000-01-01")).exists());
    assert!(dir.path().join(managed_log_name("2001-01-01")).exists());
    assert!(dir.path().join("other.log").exists());
}

fn managed_log_name(date: &str) -> String {
    format!("codex-proxy-rs.{date}.log")
}
