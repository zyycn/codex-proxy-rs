use std::{fs, io::Write, process::Command};

use chrono::{Duration, Utc};
use codex_proxy_rs::infra::{
    logging::{RotationConfig, TracingConfig, build_file_appender, init_tracing},
    time::china_date,
};

#[test]
fn tracing_config_should_reject_disabled_outputs() {
    let result = init_tracing(&TracingConfig::new("info", false, None));

    assert!(result.is_err());
}

#[test]
fn tracing_stdout_should_stay_json_with_china_time_when_not_a_terminal() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_JSON_STDOUT_LOGGING";

    if std::env::var(CASE_ENV).as_deref() == Ok("child") {
        let _guard = init_tracing(&TracingConfig::new("info", true, None)).unwrap();
        tracing::info!(probe = 42, "logging format probe");
        return;
    }

    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("infra::logging::tracing_stdout_should_stay_json_with_china_time_when_not_a_terminal")
        .arg("--nocapture")
        .env(CASE_ENV, "child")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated logging test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let event = output
        .stdout
        .split(|byte| *byte == b'\n')
        .find_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .unwrap();
    assert_eq!(
        (
            event["level"].as_str(),
            event["fields"]["message"].as_str(),
            event["fields"]["probe"].as_i64(),
            event["timestamp"]
                .as_str()
                .map(|timestamp| timestamp.ends_with("+08:00")),
        ),
        (
            Some("INFO"),
            Some("logging format probe"),
            Some(42),
            Some(true)
        )
    );
}

#[test]
fn rolling_appender_should_write_daily_codex_log_file() {
    let dir = tempfile::tempdir().unwrap();
    let config = rotation_config(dir.path(), 14, 1024, 20);
    let mut appender = build_file_appender(&config).unwrap();

    appender.write_all(b"hello\n").unwrap();
    appender.flush().unwrap();

    let names = log_names(dir.path());
    let expected = managed_log_name(&china_date(&Utc::now()), 0);
    assert!(
        names.iter().any(|name| name == &expected),
        "expected China-date codex log file {expected}, found {names:?}"
    );
}

#[test]
fn rolling_appender_should_rotate_when_size_limit_is_exceeded() {
    let dir = tempfile::tempdir().unwrap();
    let config = rotation_config(dir.path(), 14, 5, 20);
    let mut appender = build_file_appender(&config).unwrap();

    appender.write_all(b"12345").unwrap();
    appender.write_all(b"6").unwrap();
    appender.flush().unwrap();

    let date = china_date(&Utc::now());
    assert_eq!(
        fs::read(dir.path().join(managed_log_name(&date, 0))).unwrap(),
        b"12345"
    );
    assert_eq!(
        fs::read(dir.path().join(managed_log_name(&date, 1))).unwrap(),
        b"6"
    );
}

#[test]
fn rolling_appender_should_remove_logs_outside_calendar_retention() {
    let dir = tempfile::tempdir().unwrap();
    let old_date = china_date(&(Utc::now() - Duration::days(14)));
    let retained_date = china_date(&(Utc::now() - Duration::days(13)));
    fs::write(dir.path().join(managed_log_name(&old_date, 0)), b"old").unwrap();
    fs::write(
        dir.path().join(managed_log_name(&retained_date, 0)),
        b"retained",
    )
    .unwrap();
    fs::write(dir.path().join("other.log"), b"unmanaged").unwrap();
    let config = rotation_config(dir.path(), 14, 1024, 20);

    let _appender = build_file_appender(&config).unwrap();

    assert!(!dir.path().join(managed_log_name(&old_date, 0)).exists());
    assert!(
        dir.path()
            .join(managed_log_name(&retained_date, 0))
            .exists()
    );
    assert!(dir.path().join("other.log").exists());
}

#[test]
fn rolling_appender_should_enforce_global_file_count_limit() {
    let dir = tempfile::tempdir().unwrap();
    let date = china_date(&Utc::now());
    for segment in 0..3 {
        fs::write(
            dir.path().join(managed_log_name(&date, segment)),
            segment.to_string(),
        )
        .unwrap();
    }
    let config = rotation_config(dir.path(), 14, 1024, 2);

    let _appender = build_file_appender(&config).unwrap();

    let names = log_names(dir.path());
    assert_eq!(names.len(), 2);
    assert!(names.contains(&managed_log_name(&date, 2)));
    assert!(names.contains(&managed_log_name(&date, 1)));
}

#[test]
fn rolling_appender_should_reject_zero_limits() {
    let dir = tempfile::tempdir().unwrap();

    assert!(build_file_appender(&rotation_config(dir.path(), 0, 1024, 20)).is_err());
    assert!(build_file_appender(&rotation_config(dir.path(), 14, 0, 20)).is_err());
    assert!(build_file_appender(&rotation_config(dir.path(), 14, 1024, 0)).is_err());
}

fn rotation_config(
    directory: &std::path::Path,
    retention_days: usize,
    max_file_size_bytes: u64,
    max_files: usize,
) -> RotationConfig {
    RotationConfig::new(directory, retention_days, max_file_size_bytes, max_files)
}

fn log_names(directory: &std::path::Path) -> Vec<String> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("codex-proxy-rs."))
        .collect()
}

fn managed_log_name(date: &str, segment: usize) -> String {
    if segment == 0 {
        format!("codex-proxy-rs.{date}.log")
    } else {
        format!("codex-proxy-rs.{date}.{segment}.log")
    }
}
