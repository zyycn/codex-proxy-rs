use std::fs;

use codex_proxy_rs::bootstrap::{
    config::{BootstrapConfig, ConfigError, TopologyOverrides},
    services::account_pool_options_from_config,
};

const CONFIG_EXAMPLE: &str = include_str!("../../../../deploy/config.example.yaml");
const POSTGRES_PASSWORD: &str = "111111111111111111111111111111111111111111111111";
const REDIS_PASSWORD: &str = "222222222222222222222222222222222222222222222222";
const ADMIN_PASSWORD: &str = "test-admin-password";

#[test]
fn config_loader_should_load_complete_example() {
    let directory = write_config(&complete_config());

    let loaded = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap();
    let config = loaded.app();

    assert_eq!(
        (
            config.server.host.as_str(),
            config.server.port,
            config.api.base_url.as_str(),
            config.wire_profile.codex_version.as_str(),
            config.wire_profile.desktop_version.as_str(),
            config.wire_profile.desktop_build.as_str(),
            config.quota.refresh_interval_minutes,
            config.ws_pool.initial_event_timeout_ms,
            config.admin.default_username.as_str(),
            config.telemetry.enabled,
        ),
        (
            "127.0.0.1",
            8080,
            "https://chatgpt.com/backend-api",
            "0.144.2",
            "26.707.72221",
            "5307",
            5,
            20_000,
            "admin@cpr.local",
            true,
        )
    );
}

#[test]
fn config_loader_should_resolve_paths_relative_to_config_file() {
    let directory = write_config(&complete_config());

    let loaded = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap();

    assert_eq!(
        (
            loaded.app().runtime.data_directory.as_path(),
            loaded.app().logging.file.directory.as_path(),
        ),
        (
            directory.path().join("../.runtime/data").as_path(),
            directory.path().join("../.runtime/logs").as_path(),
        )
    );
}

#[test]
fn config_loader_should_inject_service_passwords_into_connection_urls() {
    let directory = write_config(&complete_config());

    let loaded = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap();

    assert_eq!(
        (loaded.database_url(), loaded.redis_url()),
        (
            "postgres://codex_proxy:111111111111111111111111111111111111111111111111@127.0.0.1:5432/codex_proxy",
            "redis://:222222222222222222222222222222222222222222222222@127.0.0.1:6379/",
        )
    );
}

#[test]
fn config_loader_should_apply_only_explicit_topology_overrides() {
    let directory = write_config(&complete_config());
    let overrides = TopologyOverrides {
        server_host: Some("0.0.0.0".to_string()),
        server_port: Some(18080),
        database_url: Some("postgres://codex_proxy@postgres:5432/codex_proxy".to_string()),
        redis_url: Some("redis://redis:6379/".to_string()),
    };

    let loaded = BootstrapConfig::load_from_path_with_overrides(
        directory.path().join("config.yaml"),
        overrides,
    )
    .unwrap();

    assert_eq!(
        (
            loaded.app().server.host.as_str(),
            loaded.app().server.port,
            loaded.app().database.url.as_str(),
            loaded.app().redis.url.as_str(),
        ),
        (
            "0.0.0.0",
            18080,
            "postgres://codex_proxy@postgres:5432/codex_proxy",
            "redis://redis:6379/",
        )
    );
}

#[test]
fn bootstrap_config_debug_should_redact_all_passwords() {
    let directory = write_config(&complete_config());
    let loaded = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap();

    let debug = format!("{loaded:?}");

    assert!(
        !debug.contains(POSTGRES_PASSWORD)
            && !debug.contains(REDIS_PASSWORD)
            && !debug.contains(ADMIN_PASSWORD)
            && debug.contains("[REDACTED]")
    );
}

#[test]
fn config_loader_should_reject_unknown_fields() {
    let yaml = complete_config().replace("    port: 8080", "    port: 8080\n    unexpected: true");
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert!(matches!(error, ConfigError::InvalidDocument { .. }));
}

#[test]
fn config_loader_should_reject_removed_tls_section() {
    let yaml = complete_config().replace(
        "  ws_pool:",
        "  tls:\n    force_http11: false\n\n  ws_pool:",
    );
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert!(matches!(error, ConfigError::InvalidDocument { .. }));
}

#[test]
fn config_loader_should_reject_missing_explicit_fields() {
    let yaml = complete_config().replacen("  telemetry:\n    enabled: true\n", "", 1);
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert!(matches!(error, ConfigError::InvalidDocument { .. }));
}

#[test]
fn config_loader_should_reject_unsupported_schema_version() {
    let yaml = complete_config().replace("schema_version: 1", "schema_version: 0");
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert_eq!(error, ConfigError::UnsupportedSchemaVersion);
}

#[test]
fn config_loader_should_reject_embedded_database_password() {
    let yaml = complete_config().replace(
        "postgres://codex_proxy@127.0.0.1",
        "postgres://codex_proxy:duplicated@127.0.0.1",
    );
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert_eq!(error, ConfigError::PasswordInUrl("database.url"));
}

#[test]
fn config_loader_should_reject_non_hex_postgres_password() {
    let yaml = complete_config().replace(POSTGRES_PASSWORD, "not-a-hex-password");
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert_eq!(
        error,
        ConfigError::InvalidServicePassword("database.password")
    );
}

#[test]
fn config_loader_should_reject_wrong_length_redis_password() {
    let yaml = complete_config().replace(REDIS_PASSWORD, "2222222222222222");
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert_eq!(error, ConfigError::InvalidServicePassword("redis.password"));
}

#[test]
fn config_loader_should_reject_weak_admin_password() {
    let yaml = complete_config().replace(ADMIN_PASSWORD, "password");
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert_eq!(error, ConfigError::WeakAdminPassword);
}

#[test]
fn config_loader_should_reject_admin_password_with_compose_interpolation() {
    let yaml = complete_config().replace(ADMIN_PASSWORD, "strong$password-value");
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert_eq!(error, ConfigError::WeakAdminPassword);
}

#[test]
fn config_loader_should_reject_legacy_fingerprint_section() {
    let yaml = complete_config().replace("  wire_profile:", "  fingerprint:");
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert!(matches!(error, ConfigError::InvalidDocument { .. }));
}

#[test]
fn config_loader_should_reject_invalid_codex_core_version() {
    let yaml = complete_config().replace("codex_version: '0.144.2'", "codex_version: 'Desktop'");
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert_eq!(
        error,
        ConfigError::InvalidField("wire_profile.codex_version")
    );
}

#[test]
fn config_loader_should_reject_disabled_all_log_outputs() {
    let yaml = complete_config()
        .replace("    stdout: true", "    stdout: false")
        .replace("      enabled: true", "      enabled: false");
    let directory = write_config(&yaml);

    let error = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap_err();

    assert_eq!(error, ConfigError::InvalidField("logging"));
}

#[test]
fn account_pool_options_should_use_quota_skip_exhausted() {
    let directory = write_config(&complete_config());
    let loaded = BootstrapConfig::load_from_path(directory.path().join("config.yaml")).unwrap();
    let mut config = loaded.app().clone();
    config.quota.skip_exhausted = false;

    let options = account_pool_options_from_config(&config);

    assert!(!options.skip_quota_limited);
}

fn complete_config() -> String {
    CONFIG_EXAMPLE
        .replace(
            "password: &postgres_password ''",
            &format!("password: &postgres_password '{POSTGRES_PASSWORD}'"),
        )
        .replace(
            "password: &redis_password ''",
            &format!("password: &redis_password '{REDIS_PASSWORD}'"),
        )
        .replace(
            "default_password: ''",
            &format!("default_password: '{ADMIN_PASSWORD}'"),
        )
}

fn write_config(yaml: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("config.yaml"), yaml).unwrap();
    directory
}
