use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use gateway_host::config::{FileLoggingConfig, HostConfig, ListenConfig, LoggingConfig};
use gateway_host::system_update::SystemUpdateConfig;

mod sink;
mod writer;

const LOG_DIRECTORY_ENV: &str = "CPR_LOGGING_TEST_DIRECTORY";
const CHILD_PROCESS_ENV: &str = "CPR_LOGGING_TEST_CHILD";
const REQUEST_DUMP_ENABLED_ENV: &str = "CPR_LOGGING_TEST_REQUEST_DUMP_ENABLED";
const APPLICATION_LOG_FILE_PREFIX: &str = "codex-proxy-rs-application.";
const OAUTH_RECOVERY_LOG_FILE_PREFIX: &str = "codex-proxy-rs-oauth-recovery.";
const REQUEST_DUMP_LOG_FILE_PREFIX: &str = "codex-proxy-rs-request-dump.";
const APPLICATION_LOG_TARGET: &str = "logging_test_application";
const APPLICATION_LOG_MARKER: &str = "application-file-filter-test";
const OAUTH_RECOVERY_LOG_TARGET: &str = "oauth_recovery";
const OAUTH_RECOVERY_LOG_MARKER: &str = "oauth-recovery-file-filter-test";
const OAUTH_RECOVERY_PROVIDER: &str = "openai";
const REQUEST_DUMP_LOG_TARGET: &str = "request_dump";
const REQUEST_DUMP_LOG_MARKER: &str = "request-dump-file-filter-test";
const REQUEST_DUMP_SECRET: &str = "unredacted-authorization-value";

#[test]
fn logging_requires_at_least_one_sink() {
    let mut config = HostConfig {
        listen: ListenConfig {
            host: "127.0.0.1".to_owned(),
            port: 8080,
        },
        runtime_data_dir: PathBuf::from("/tmp/runtime-data"),
        logging: LoggingConfig {
            level: "info".to_owned(),
            stdout: false,
            file: FileLoggingConfig {
                enabled: false,
                directory: PathBuf::from("logs"),
                retention_days: 7,
                max_file_size_mb: 100,
            },
            oauth_recovery: false,
            request_dump: false,
            request_dump_retention_days: 1,
        },
        system_update: SystemUpdateConfig::default(),
        drain_timeout_seconds: 30,
        worker_shutdown_timeout_seconds: 30,
    };

    assert!(
        config
            .resolve_and_validate(
                std::path::Path::new("/tmp"),
                std::path::Path::new("/tmp/web/dist"),
            )
            .is_err()
    );
}

#[test]
fn sensitive_file_logging_is_separate_and_overrides_global_log_level() {
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        write_sensitive_logs(
            PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).expect("child log directory")),
            env::var(REQUEST_DUMP_ENABLED_ENV).is_ok_and(|value| value == "true"),
        );
        return;
    }

    let directory = tempfile::tempdir().expect("create log directory");
    let output = Command::new(env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "logging::sensitive_file_logging_is_separate_and_overrides_global_log_level",
            "--nocapture",
        ])
        .env(CHILD_PROCESS_ENV, "1")
        .env(LOG_DIRECTORY_ENV, directory.path())
        .env(REQUEST_DUMP_ENABLED_ENV, "true")
        .env(
            "RUST_LOG",
            "off,logging_test_application=info,oauth_recovery=off,request_dump=trace",
        )
        .output()
        .expect("run logging child process");

    assert!(
        output.status.success(),
        "logging child process failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let application_log = read_log_file_set(directory.path(), APPLICATION_LOG_FILE_PREFIX);
    assert!(application_log.contains(&json_target_field(APPLICATION_LOG_TARGET)));
    assert!(application_log.contains(APPLICATION_LOG_MARKER));
    assert!(!application_log.contains(&json_target_field(OAUTH_RECOVERY_LOG_TARGET)));
    assert!(!application_log.contains(OAUTH_RECOVERY_LOG_MARKER));
    assert!(!application_log.contains(&json_target_field(REQUEST_DUMP_LOG_TARGET)));
    assert!(!application_log.contains(REQUEST_DUMP_SECRET));

    let recovery_log = read_log_file_set(directory.path(), OAUTH_RECOVERY_LOG_FILE_PREFIX);
    assert!(recovery_log.contains(&json_target_field(OAUTH_RECOVERY_LOG_TARGET)));
    assert!(recovery_log.contains(OAUTH_RECOVERY_LOG_MARKER));
    assert!(!recovery_log.contains(&json_target_field(APPLICATION_LOG_TARGET)));
    assert!(!recovery_log.contains(APPLICATION_LOG_MARKER));
    assert!(recovery_log.contains(&format!(r#""provider":"{OAUTH_RECOVERY_PROVIDER}""#)));
    assert!(!recovery_log.contains(&json_target_field(REQUEST_DUMP_LOG_TARGET)));
    assert!(!recovery_log.contains(REQUEST_DUMP_SECRET));

    let request_dump_log = read_log_file_set(directory.path(), REQUEST_DUMP_LOG_FILE_PREFIX);
    assert!(request_dump_log.contains(&json_target_field(REQUEST_DUMP_LOG_TARGET)));
    assert!(request_dump_log.contains(REQUEST_DUMP_LOG_MARKER));
    assert!(request_dump_log.contains(REQUEST_DUMP_SECRET));
    assert!(!request_dump_log.contains(&json_target_field(APPLICATION_LOG_TARGET)));
    assert!(!request_dump_log.contains(APPLICATION_LOG_MARKER));
    assert!(!request_dump_log.contains(&json_target_field(OAUTH_RECOVERY_LOG_TARGET)));
    assert!(!request_dump_log.contains(OAUTH_RECOVERY_LOG_MARKER));

    let disabled_directory = tempfile::tempdir().expect("create disabled log directory");
    let output = Command::new(env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "logging::sensitive_file_logging_is_separate_and_overrides_global_log_level",
            "--nocapture",
        ])
        .env(CHILD_PROCESS_ENV, "1")
        .env(LOG_DIRECTORY_ENV, disabled_directory.path())
        .env(REQUEST_DUMP_ENABLED_ENV, "false")
        .env(
            "RUST_LOG",
            "off,logging_test_application=info,oauth_recovery=off,request_dump=trace",
        )
        .output()
        .expect("run logging child process with request dump disabled");
    assert!(
        output.status.success(),
        "logging child process failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(!log_file_set_exists(
        disabled_directory.path(),
        REQUEST_DUMP_LOG_FILE_PREFIX
    ));
    let application_log = read_log_file_set(disabled_directory.path(), APPLICATION_LOG_FILE_PREFIX);
    assert!(!application_log.contains(REQUEST_DUMP_SECRET));
}

#[test]
fn oauth_recovery_switch_controls_only_its_dedicated_file() {
    const OAUTH_ENABLED_ENV: &str = "CPR_LOGGING_TEST_OAUTH_ENABLED";
    const APPLICATION_ENABLED_ENV: &str = "CPR_LOGGING_TEST_APPLICATION_ENABLED";
    const TOKEN: &str = "synthetic-oauth-token-must-stay-in-recovery-file";
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let mut config =
            logging_config(PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap()), true);
        config.logging.oauth_recovery = env::var(OAUTH_ENABLED_ENV).unwrap() == "true";
        config.logging.file.enabled = env::var(APPLICATION_ENABLED_ENV).unwrap() == "true";
        config.logging.stdout = true;
        with_logging(config, || {
            tracing::info!(target: APPLICATION_LOG_TARGET, marker = APPLICATION_LOG_MARKER);
            tracing::info!(target: OAUTH_RECOVERY_LOG_TARGET, access_token = TOKEN);
            tracing::info!(target: REQUEST_DUMP_LOG_TARGET, marker = REQUEST_DUMP_LOG_MARKER);
        });
        return;
    }
    for application_enabled in [false, true] {
        for oauth_enabled in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let output = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "logging::oauth_recovery_switch_controls_only_its_dedicated_file",
                    "--nocapture",
                ])
                .env(CHILD_PROCESS_ENV, "1")
                .env(LOG_DIRECTORY_ENV, directory.path())
                .env(OAUTH_ENABLED_ENV, oauth_enabled.to_string())
                .env(APPLICATION_ENABLED_ENV, application_enabled.to_string())
                .env("RUST_LOG", "info,oauth_recovery=trace,request_dump=trace")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
            assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
            assert_eq!(
                log_file_set_exists(directory.path(), OAUTH_RECOVERY_LOG_FILE_PREFIX),
                oauth_enabled
            );
            assert_eq!(
                log_file_set_exists(directory.path(), APPLICATION_LOG_FILE_PREFIX),
                application_enabled
            );
            if oauth_enabled {
                assert!(
                    read_log_file_set(directory.path(), OAUTH_RECOVERY_LOG_FILE_PREFIX)
                        .contains(TOKEN)
                );
            }
            if application_enabled {
                let body = read_log_file_set(directory.path(), APPLICATION_LOG_FILE_PREFIX);
                assert!(body.contains(APPLICATION_LOG_MARKER));
                assert!(!body.contains(TOKEN));
            } else {
                assert!(String::from_utf8_lossy(&output.stdout).contains(APPLICATION_LOG_MARKER));
            }
            let dump = read_log_file_set(directory.path(), REQUEST_DUMP_LOG_FILE_PREFIX);
            assert!(dump.contains(REQUEST_DUMP_LOG_MARKER));
            assert!(!dump.contains(TOKEN));
        }
    }
}

fn write_sensitive_logs(directory: PathBuf, request_dump: bool) {
    with_file_logging(directory, request_dump, || {
        tracing::info!(
            target: APPLICATION_LOG_TARGET,
            marker = APPLICATION_LOG_MARKER,
            "application file test record"
        );
        tracing::info!(
            target: OAUTH_RECOVERY_LOG_TARGET,
            provider = OAUTH_RECOVERY_PROVIDER,
            marker = OAUTH_RECOVERY_LOG_MARKER,
            "OAuth recovery test record"
        );
        tracing::info!(
            target: REQUEST_DUMP_LOG_TARGET,
            marker = REQUEST_DUMP_LOG_MARKER,
            authorization = REQUEST_DUMP_SECRET,
            "request dump test record"
        );
    });
}

fn with_file_logging(directory: PathBuf, request_dump: bool, write: impl FnOnce()) {
    with_logging(logging_config(directory, request_dump), write);
}

fn with_logging(config: HostConfig, write: impl FnOnce()) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create logging runtime");
    let bundle = runtime
        .block_on(gateway_host::initialize(config))
        .expect("initialize logging");
    write();
    drop(bundle);
}

fn logging_config(directory: PathBuf, request_dump: bool) -> HostConfig {
    HostConfig {
        listen: ListenConfig {
            host: "127.0.0.1".to_owned(),
            port: 8080,
        },
        runtime_data_dir: PathBuf::from("/tmp/runtime-data"),
        logging: LoggingConfig {
            level: "off".to_owned(),
            stdout: false,
            file: FileLoggingConfig {
                enabled: true,
                directory,
                retention_days: 7,
                max_file_size_mb: 1,
            },
            oauth_recovery: true,
            request_dump,
            request_dump_retention_days: 1,
        },
        system_update: SystemUpdateConfig::default(),
        drain_timeout_seconds: 30,
        worker_shutdown_timeout_seconds: 30,
    }
}

fn read_log_file_set(directory: &Path, file_prefix: &str) -> String {
    let mut files = fs::read_dir(directory)
        .expect("read log directory")
        .map(|entry| entry.expect("read log entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(file_prefix)
                        && (name.ends_with(".log") || name.ends_with(".log.gz"))
                })
        })
        .collect::<Vec<_>>();
    files.sort();

    assert!(!files.is_empty(), "expected log file set {file_prefix}");
    files
        .into_iter()
        .map(|path| {
            if path.extension().is_some_and(|extension| extension == "gz") {
                let mut body = String::new();
                flate2::read::GzDecoder::new(fs::File::open(path).expect("open archive"))
                    .read_to_string(&mut body)
                    .expect("read complete archive");
                body
            } else {
                fs::read_to_string(path).expect("read log file")
            }
        })
        .collect()
}

fn log_file_set_exists(directory: &Path, file_prefix: &str) -> bool {
    fs::read_dir(directory)
        .expect("read log directory")
        .filter_map(Result::ok)
        .any(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.starts_with(file_prefix)
                    && (name.ends_with(".log") || name.ends_with(".log.gz"))
            })
        })
}

fn json_target_field(target: &str) -> String {
    format!(r#""target":"{target}""#)
}
