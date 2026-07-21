use std::{fs, process::Command};

use codex_proxy_rs::bootstrap::GatewayConfig;
use gateway_host::LoadableConfig;

const CONFIG_EXAMPLE: &str = include_str!("../../../../deploy/config.example.yaml");
const POSTGRES_PASSWORD: &str = "111111111111111111111111111111111111111111111111";
const REDIS_PASSWORD: &str = "222222222222222222222222222222222222222222222222";
const ADMIN_PASSWORD: &str = "test-admin-password";
const TOPOLOGY_CHILD_ENV: &str = "CPR_TEST_TOPOLOGY_CHILD";

#[test]
fn config_loader_should_load_complete_terminal_example() {
    parse_config(&valid_config()).expect("terminal config example");
}

#[test]
fn config_loader_should_resolve_paths_relative_to_config_file() {
    let (config, _directory) = parse_config(&valid_config()).expect("resolved config");
    let debug = format!("{config:?}");
    assert!(debug.contains(".runtime/logs"));
    assert!(debug.contains("frontend/dist"));
}

#[test]
fn config_loader_should_inject_connection_passwords_into_urls() {
    parse_config(&valid_config()).expect("Store validates password injection into both URLs");
}

#[test]
fn config_loader_should_apply_only_explicit_topology_overrides() {
    let invalid = valid_config()
        .replace("host: '127.0.0.1'", "host: ''")
        .replace("port: 8080", "port: 0")
        .replace(
            "url: 'postgres://codex_proxy@127.0.0.1:5432/codex_proxy'",
            "url: 'invalid-postgres-url'",
        )
        .replace("url: 'redis://127.0.0.1:6379/'", "url: 'invalid-redis-url'")
        .replace(POSTGRES_PASSWORD, "invalid-postgres-password")
        .replace(REDIS_PASSWORD, "invalid-redis-password");
    if std::env::var_os(TOPOLOGY_CHILD_ENV).is_some() {
        parse_config(&invalid).expect("explicit package-owned environment overrides");
        return;
    }
    assert!(parse_config(&invalid).is_err());
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "bootstrap::config_loader_should_apply_only_explicit_topology_overrides",
        ])
        .env(TOPOLOGY_CHILD_ENV, "1")
        .env("CPR_SERVER_HOST", "127.0.0.1")
        .env("CPR_SERVER_PORT", "8080")
        .env(
            "CPR_DATABASE_URL",
            "postgres://codex_proxy@127.0.0.1:5432/codex_proxy",
        )
        .env("CPR_REDIS_URL", "redis://127.0.0.1:6379/")
        .env("CPR_DATABASE_PASSWORD", POSTGRES_PASSWORD)
        .env("CPR_REDIS_PASSWORD", REDIS_PASSWORD)
        .status()
        .expect("run isolated environment override test");
    assert!(status.success());
}

#[test]
fn bootstrap_config_debug_should_redact_all_passwords() {
    let (config, _directory) = parse_config(&valid_config()).expect("config");
    let debug = format!("{config:?}");
    assert!(!debug.contains(POSTGRES_PASSWORD));
    assert!(!debug.contains(REDIS_PASSWORD));
    assert!(!debug.contains(ADMIN_PASSWORD));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn config_loader_should_reject_unknown_fields() {
    assert_rejected(format!(
        "{}\nunknown_terminal_field: true\n",
        valid_config()
    ));
}

#[test]
fn config_loader_should_reject_removed_tls_section() {
    assert_rejected(valid_config().replace("openai:\n", "openai:\n  tls: {}\n"));
}

#[test]
fn config_loader_should_reject_missing_explicit_fields() {
    assert_rejected(valid_config().replace("  request_id_header: 'x-request-id'\n", ""));
}

#[test]
fn config_loader_should_reject_unsupported_schema_version() {
    assert_rejected(valid_config().replace("schema_version: 1", "schema_version: 2"));
}

#[test]
fn config_loader_should_reject_embedded_database_password() {
    assert_rejected(valid_config().replace(
        "postgres://codex_proxy@127.0.0.1:5432/codex_proxy",
        "postgres://codex_proxy:embedded@127.0.0.1:5432/codex_proxy",
    ));
}

#[test]
fn config_loader_should_reject_non_hex_postgres_password() {
    assert_rejected(valid_config().replace(POSTGRES_PASSWORD, &"g".repeat(48)));
}

#[test]
fn config_loader_should_reject_wrong_length_redis_password() {
    assert_rejected(valid_config().replace(REDIS_PASSWORD, "1234"));
}

#[test]
fn config_loader_should_reject_weak_admin_password() {
    assert_rejected(valid_config().replace(ADMIN_PASSWORD, "password"));
}

#[test]
fn config_loader_should_reject_admin_password_with_compose_interpolation() {
    assert_rejected(valid_config().replace(ADMIN_PASSWORD, "unsafe$password"));
}

#[test]
fn config_loader_should_reject_removed_fingerprint_section() {
    assert_rejected(valid_config().replace(
        "openai:\n",
        "openai:\n  fingerprint:\n    browser: removed\n",
    ));
}

#[test]
fn config_loader_should_reject_invalid_codex_core_version() {
    assert_rejected(valid_config().replace("codex_version: '0.144.6'", "codex_version: 'latest'"));
}

#[test]
fn config_loader_should_reject_disabled_all_log_outputs() {
    assert_rejected(
        valid_config()
            .replace("stdout: true", "stdout: false")
            .replace("enabled: true", "enabled: false"),
    );
}

#[test]
fn config_loader_should_reject_zero_server_port() {
    assert_rejected(valid_config().replace("port: 8080", "port: 0"));
}

#[test]
fn config_loader_should_reject_invalid_desktop_profile_fields() {
    assert_rejected(valid_config().replace("desktop_build: '5628'", "desktop_build: 'build'"));
}

fn assert_rejected(config: String) {
    assert!(parse_config(&config).is_err());
}

fn valid_config() -> String {
    CONFIG_EXAMPLE
        .replacen(
            "password: &postgres_password ''",
            &format!("password: &postgres_password '{POSTGRES_PASSWORD}'"),
            1,
        )
        .replacen(
            "password: &redis_password ''",
            &format!("password: &redis_password '{REDIS_PASSWORD}'"),
            1,
        )
        .replace(
            "default_password: ''",
            &format!("default_password: '{ADMIN_PASSWORD}'"),
        )
}

fn parse_config(config: &str) -> Result<(GatewayConfig, tempfile::TempDir), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let deploy = directory.path().join("deploy");
    fs::create_dir(&deploy).map_err(|error| error.to_string())?;
    let path = deploy.join("config.yaml");
    fs::write(&path, config).map_err(|error| error.to_string())?;
    let mut config = config::Config::builder()
        .add_source(config::File::from(path).required(true))
        .build()
        .and_then(config::Config::try_deserialize::<GatewayConfig>)
        .map_err(|error| error.to_string())?;
    config
        .resolve_and_validate(&deploy)
        .map_err(|error| error.to_string())?;
    Ok((config, directory))
}
