use codex_proxy_rs::bootstrap::config::LoggingConfig;
use codex_proxy_rs::bootstrap::config::{AdminConfig, AdminConfigError, AppConfig};
use codex_proxy_rs::bootstrap::services::account_pool_options_from_config;
use serde::de::DeserializeOwned;
use std::{fs, process::Command};

const DEFAULT_CONFIG_YAML: &str = r#"
server:
  host: 0.0.0.0
  port: 8080
api:
  base_url: https://chatgpt.com/backend-api
database:
  url: postgres://codex_proxy:codex_proxy@127.0.0.1:5432/codex_proxy
redis:
  url: redis://127.0.0.1:6379
quota:
  refresh_interval_minutes: 5
  skip_exhausted: true
tls:
  force_http11: false
ws_pool:
  enabled: true
  max_age_ms: 3300000
  max_per_account: 8
fingerprint:
  originator: Codex Desktop
  app_version: 26.519.81530
  build_number: "3178"
  platform: darwin
  arch: arm64
  chromium_version: "146"
  user_agent_template: "Codex Desktop/{version} ({platform}; {arch})"
  default_headers:
    - name: Accept-Encoding
      value: gzip, deflate, br, zstd
    - name: Accept-Language
      value: en-US,en;q=0.9
    - name: sec-ch-ua-mobile
      value: "?0"
    - name: sec-ch-ua-platform
      value: "\"macOS\""
    - name: sec-fetch-site
      value: same-origin
    - name: sec-fetch-mode
      value: cors
    - name: sec-fetch-dest
      value: empty
  header_order:
    - authorization
    - chatgpt-account-id
    - originator
    - x-openai-internal-codex-residency
    - x-client-request-id
    - x-codex-installation-id
    - x-codex-turn-state
    - openai-beta
    - user-agent
    - sec-ch-ua
    - sec-ch-ua-mobile
    - sec-ch-ua-platform
    - accept-encoding
    - accept-language
    - sec-fetch-site
    - sec-fetch-mode
    - sec-fetch-dest
    - content-type
    - accept
    - cookie
admin:
  session_ttl_minutes: 1440
  default_username: admin
  default_password: test-admin-password
logging:
  level: info
  stdout: true
  file:
    enabled: true
    directory: .runtime/logs
    retention_days: 14
    max_file_size_mb: 20
    max_files: 20
telemetry:
  enabled: true
"#;

#[test]
fn default_config_keeps_only_codex_backend() {
    let cfg: AppConfig = parse_yaml_config(DEFAULT_CONFIG_YAML).unwrap();
    assert_eq!(cfg.server.host, "0.0.0.0");
    assert_eq!(cfg.api.base_url, "https://chatgpt.com/backend-api");
    assert!(cfg.model_aliases.is_empty());
    assert_eq!(cfg.auth.refresh_margin_seconds, 3600);
    assert_eq!(cfg.auth.rotation_strategy, "smart");
    assert_eq!(cfg.auth.oauth_client_id, "app_EMoamEEZ73f0CkXaXp7hrann");
    assert_eq!(
        cfg.auth.oauth_token_endpoint,
        "https://auth.openai.com/oauth/token"
    );
    assert_eq!(cfg.quota.refresh_interval_minutes, 5);
    assert!(cfg.quota.skip_exhausted);
    assert!(cfg.ws_pool.enabled);
    assert_eq!(cfg.ws_pool.max_age_ms, 3_300_000);
    assert_eq!(cfg.ws_pool.max_per_account, 8);
    assert_eq!(cfg.fingerprint.originator, "Codex Desktop");
    assert_eq!(cfg.fingerprint.app_version, "26.519.81530");
    assert_eq!(cfg.fingerprint.default_headers[0].name, "Accept-Encoding");
}

#[test]
fn default_config_keeps_runtime_artifacts_under_runtime_directory() {
    let cfg: AppConfig = parse_yaml_config(DEFAULT_CONFIG_YAML).unwrap();

    assert_eq!(
        cfg.database.url,
        "postgres://codex_proxy:codex_proxy@127.0.0.1:5432/codex_proxy"
    );
    assert_eq!(cfg.redis.url, "redis://127.0.0.1:6379");
    assert_eq!(cfg.logging.file.directory, ".runtime/logs");
    assert!(cfg.telemetry.enabled);
}

#[test]
fn account_pool_options_should_use_quota_skip_exhausted() {
    let mut cfg: AppConfig = parse_yaml_config(DEFAULT_CONFIG_YAML).unwrap();
    cfg.quota.skip_exhausted = false;

    let options = account_pool_options_from_config(&cfg);

    assert!(!options.skip_quota_limited);
}

#[test]
fn admin_config_should_reject_weak_default_password() {
    let config = AdminConfig {
        default_password: "123456".to_string(),
        ..AdminConfig::default()
    };

    assert_eq!(
        config.validate_default_password(),
        Err(AdminConfigError::WeakDefaultPassword)
    );
}

#[test]
fn admin_config_should_accept_explicit_strong_default_password() {
    let config = AdminConfig {
        default_password: "correct-horse-battery-staple".to_string(),
        ..AdminConfig::default()
    };

    assert_eq!(config.validate_default_password(), Ok(()));
}

#[test]
fn config_should_reject_unknown_top_level_sections() {
    let err = parse_yaml_config::<AppConfig>(
        r"
server:
  host: 127.0.0.1
  port: 8080
api:
  base_url: https://chatgpt.com/backend-api
unexpected: {}
database:
  url: postgres://codex_proxy:codex_proxy@127.0.0.1:5432/codex_proxy
redis:
  url: redis://127.0.0.1:6379
",
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("unexpected"),
        "expected unknown section to be rejected, got {err}"
    );
}

#[test]
fn config_loader_should_read_only_config_yaml() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_CONFIG_FILE_ONLY_CASE";
    if std::env::var(CASE_ENV).as_deref() != Ok("child") {
        run_isolated_config_test("config_loader_should_read_only_config_yaml", |command| {
            command.env(CASE_ENV, "child");
        });
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.yaml"),
        r#"
server:
  host: 127.0.0.1
  port: 8080
api:
  base_url: https://chatgpt.com/backend-api
database:
  url: postgres://codex_proxy:codex_proxy@127.0.0.1:5432/codex_proxy
redis:
  url: redis://127.0.0.1:6379
quota:
  skip_exhausted: false
tls:
  force_http11: false
ws_pool:
  enabled: true
  max_age_ms: 3300000
  max_per_account: 8
fingerprint:
  originator: Codex Desktop
  app_version: 26.519.81530
  build_number: "3178"
  platform: darwin
  arch: arm64
  chromium_version: "146"
  user_agent_template: "Codex Desktop/{version} ({platform}; {arch})"
  default_headers:
    - name: Accept-Encoding
      value: gzip, deflate, br, zstd
    - name: Accept-Language
      value: en-US,en;q=0.9
    - name: sec-ch-ua-mobile
      value: "?0"
    - name: sec-ch-ua-platform
      value: "\"macOS\""
    - name: sec-fetch-site
      value: same-origin
    - name: sec-fetch-mode
      value: cors
    - name: sec-fetch-dest
      value: empty
  header_order:
    - authorization
    - chatgpt-account-id
    - originator
    - x-openai-internal-codex-residency
    - x-client-request-id
    - x-codex-installation-id
    - x-codex-turn-state
    - openai-beta
    - user-agent
    - sec-ch-ua
    - sec-ch-ua-mobile
    - sec-ch-ua-platform
    - accept-encoding
    - accept-language
    - sec-fetch-site
    - sec-fetch-mode
    - sec-fetch-dest
    - content-type
    - accept
    - cookie
admin:
  session_ttl_minutes: 1440
logging:
  level: info
  stdout: true
  file:
    enabled: true
    directory: .runtime/logs
    retention_days: 14
    max_file_size_mb: 20
    max_files: 20
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("ignored-extra.yaml"),
        r"
server:
  host: 0.0.0.0
logging:
  directory: ignored-logs
ws_pool:
  enabled: false
  max_age_ms: 120000
  max_per_account: 2
",
    )
    .unwrap();

    let cfg = AppConfig::load_from_dir(dir.path()).unwrap();

    assert_eq!(cfg.server.host, "127.0.0.1");
    assert_eq!(cfg.server.port, 8080);
    assert_eq!(
        cfg.database.url,
        "postgres://codex_proxy:codex_proxy@127.0.0.1:5432/codex_proxy"
    );
    assert_eq!(cfg.redis.url, "redis://127.0.0.1:6379");
    assert!(cfg.model_aliases.is_empty());
    assert_eq!(cfg.auth.max_concurrent_per_account, 3);
    assert_eq!(cfg.quota.refresh_interval_minutes, 5);
    assert!(!cfg.quota.skip_exhausted);
    assert_eq!(cfg.auth.oauth_client_id, "app_EMoamEEZ73f0CkXaXp7hrann");
    assert_eq!(
        cfg.auth.oauth_token_endpoint,
        "https://auth.openai.com/oauth/token"
    );
    assert!(cfg.ws_pool.enabled);
    assert_eq!(cfg.ws_pool.max_age_ms, 3_300_000);
    assert_eq!(cfg.ws_pool.max_per_account, 8);
    assert_eq!(cfg.logging.file.directory, ".runtime/logs");
    assert_eq!(cfg.logging.file.retention_days, 14);
    assert_eq!(cfg.logging.file.max_file_size_mb, 20);
    assert_eq!(cfg.logging.file.max_files, 20);
}

#[test]
fn server_config_should_reject_unknown_fields() {
    let err = parse_yaml_config::<AppConfig>(
        &DEFAULT_CONFIG_YAML.replace("  port: 8080", "  port: 8080\n  trusted_proxies: []"),
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("trusted_proxies"),
        "expected unknown server field to be rejected, got {err}"
    );
}

#[test]
fn config_loader_should_read_explicit_config_file_from_env() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_EXPLICIT_CONFIG_FILE_CASE";
    if std::env::var(CASE_ENV).as_deref() == Ok("child") {
        let cfg = AppConfig::load().unwrap();
        assert_eq!(cfg.server.host, "127.0.0.2");
        assert_eq!(cfg.logging.file.directory, ".runtime/env-logs");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("custom-config.yaml");
    fs::write(
        &config_file,
        DEFAULT_CONFIG_YAML
            .replace("host: 0.0.0.0", "host: 127.0.0.2")
            .replace("directory: .runtime/logs", "directory: .runtime/env-logs"),
    )
    .unwrap();
    run_isolated_config_test(
        "config_loader_should_read_explicit_config_file_from_env",
        |command| {
            command
                .env(CASE_ENV, "child")
                .env("CPR_CONFIG_FILE", &config_file);
        },
    );
}

#[test]
fn config_loader_should_override_runtime_passwords_from_env() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_CONFIG_PASSWORD_OVERRIDE_CASE";
    if std::env::var(CASE_ENV).as_deref() != Ok("child") {
        run_isolated_config_test(
            "config_loader_should_override_runtime_passwords_from_env",
            |command| {
                command
                    .env(CASE_ENV, "child")
                    .env("CPR_ADMIN_DEFAULT_PASSWORD", "admin @:/?#% password")
                    .env("CPR_POSTGRES_PASSWORD", "pg @:/?#%")
                    .env("CPR_REDIS_PASSWORD", "redis @:/?#%");
            },
        );
        return;
    }

    let dir = write_default_config();
    let cfg = AppConfig::load_from_dir(dir.path()).unwrap();

    assert_eq!(
        cfg.database.url,
        "postgres://codex_proxy:pg%20%40%3A%2F%3F%23%25@127.0.0.1:5432/codex_proxy"
    );
    assert_eq!(
        cfg.redis.url,
        "redis://:redis%20%40%3A%2F%3F%23%25@127.0.0.1:6379"
    );
    assert_eq!(cfg.admin.default_password, "admin @:/?#% password");
}

#[test]
fn config_loader_should_reject_empty_password_env() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_EMPTY_CONFIG_PASSWORD_CASE";
    if std::env::var(CASE_ENV).as_deref() != Ok("child") {
        run_isolated_config_test(
            "config_loader_should_reject_empty_password_env",
            |command| {
                command
                    .env(CASE_ENV, "child")
                    .env("CPR_POSTGRES_PASSWORD", "");
            },
        );
        return;
    }

    let dir = write_default_config();
    let error = AppConfig::load_from_dir(dir.path()).unwrap_err();

    assert_eq!(error.to_string(), "CPR_POSTGRES_PASSWORD must not be empty");
}

#[test]
fn config_loader_should_reject_empty_admin_password_env() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_EMPTY_ADMIN_PASSWORD_CASE";
    if std::env::var(CASE_ENV).as_deref() != Ok("child") {
        run_isolated_config_test(
            "config_loader_should_reject_empty_admin_password_env",
            |command| {
                command
                    .env(CASE_ENV, "child")
                    .env("CPR_ADMIN_DEFAULT_PASSWORD", "");
            },
        );
        return;
    }

    let dir = write_default_config();
    let error = AppConfig::load_from_dir(dir.path()).unwrap_err();

    assert_eq!(
        error.to_string(),
        "CPR_ADMIN_DEFAULT_PASSWORD must not be empty"
    );
}

#[test]
fn config_loader_should_not_expose_password_when_url_is_invalid() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_INVALID_CONFIG_URL_CASE";
    const SECRET: &str = "do-not-leak-this-password";
    if std::env::var(CASE_ENV).as_deref() != Ok("child") {
        run_isolated_config_test(
            "config_loader_should_not_expose_password_when_url_is_invalid",
            |command| {
                command
                    .env(CASE_ENV, "child")
                    .env("CPR_REDIS_PASSWORD", SECRET);
            },
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.yaml"),
        DEFAULT_CONFIG_YAML.replace("redis://127.0.0.1:6379", "not a URL"),
    )
    .unwrap();
    let error = AppConfig::load_from_dir(dir.path()).unwrap_err();
    let message = error.to_string();

    assert!(message.contains("redis.url"));
    assert!(message.contains("CPR_REDIS_PASSWORD"));
    assert!(!message.contains(SECRET));
}

#[test]
fn logging_config_should_reject_unknown_fields() {
    let err = parse_yaml_config::<LoggingConfig>(
        r"
level: info
stdout: true
unexpected: true
file:
  enabled: false
  directory: .runtime/logs
  retention_days: 14
  max_file_size_mb: 20
  max_files: 20
",
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("unexpected"),
        "expected unknown logging field to be rejected, got {err}"
    );
}

fn parse_yaml_config<T: DeserializeOwned>(yaml: &str) -> Result<T, config::ConfigError> {
    config::Config::builder()
        .add_source(config::File::from_str(yaml, config::FileFormat::Yaml))
        .build()?
        .try_deserialize()
}

fn write_default_config() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("config.yaml"), DEFAULT_CONFIG_YAML).unwrap();
    dir
}

fn run_isolated_config_test(test_name: &str, configure: impl FnOnce(&mut Command)) {
    let current_exe = std::env::current_exe().expect("current test binary path");
    let mut command = Command::new(current_exe);
    command
        .arg("--exact")
        .arg(format!("bootstrap::config::{test_name}"))
        .arg("--nocapture")
        .env_remove("CPR_CONFIG_FILE")
        .env_remove("CPR_ADMIN_DEFAULT_PASSWORD")
        .env_remove("CPR_POSTGRES_PASSWORD")
        .env_remove("CPR_REDIS_PASSWORD");
    configure(&mut command);

    let output = command.output().expect("run isolated config test case");
    assert!(
        output.status.success(),
        "isolated config test {test_name} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
