use codex_proxy_rs::bootstrap::config::LoggingConfig;
use codex_proxy_rs::bootstrap::config::{AdminConfig, AdminConfigError, AppConfig};
use codex_proxy_rs::bootstrap::services::account_pool_options_from_config;
use serde::de::DeserializeOwned;
use std::fs;

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
  directory: .runtime/logs
  retention_days: 14
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
    assert_eq!(cfg.logging.directory, ".runtime/logs");
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
  directory: .runtime/logs
  retention_days: 14
  enabled: false
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
    assert_eq!(cfg.logging.directory, ".runtime/logs");
    assert_eq!(cfg.logging.retention_days, 14);
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
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("custom-config.yaml");
    fs::write(
        &config_file,
        DEFAULT_CONFIG_YAML
            .replace("host: 0.0.0.0", "host: 127.0.0.2")
            .replace("directory: .runtime/logs", "directory: .runtime/env-logs"),
    )
    .unwrap();
    std::env::set_var("CPR_CONFIG_FILE", &config_file);

    let cfg = AppConfig::load().unwrap();

    std::env::remove_var("CPR_CONFIG_FILE");
    assert_eq!(cfg.server.host, "127.0.0.2");
    assert_eq!(cfg.logging.directory, ".runtime/env-logs");
}

#[test]
fn logging_config_should_reject_unknown_fields() {
    let err = parse_yaml_config::<LoggingConfig>(
        r"
directory: .runtime/logs
unexpected: true
retention_days: 14
enabled: false
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
