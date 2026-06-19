use codex_proxy_platform::config::{AppConfig, LoggingConfig};
use std::fs;

#[test]
fn default_config_keeps_only_codex_backend() {
    let cfg: AppConfig = serde_yml::from_str(include_str!("../../../config.yaml")).unwrap();
    assert_eq!(cfg.server.host, "0.0.0.0");
    assert_eq!(cfg.api.base_url, "https://chatgpt.com/backend-api");
    assert_eq!(cfg.model.default_model, "gpt-5.5");
    assert_eq!(cfg.auth.refresh_margin_seconds, 300);
    assert_eq!(cfg.auth.rotation_strategy, "least_used");
    assert_eq!(cfg.auth.oauth_client_id, "app_EMoamEEZ73f0CkXaXp7hrann");
    assert_eq!(
        cfg.auth.oauth_auth_endpoint,
        "https://auth.openai.com/oauth/authorize"
    );
    assert_eq!(
        cfg.auth.oauth_token_endpoint,
        "https://auth.openai.com/oauth/token"
    );
    assert!(cfg.ws_pool.enabled);
    assert_eq!(cfg.ws_pool.max_age_ms, 3_300_000);
    assert_eq!(cfg.ws_pool.max_per_account, 8);
}

#[test]
fn default_config_keeps_runtime_artifacts_under_runtime_directory() {
    let cfg: AppConfig = serde_yml::from_str(include_str!("../../../config.yaml")).unwrap();

    assert_eq!(
        cfg.database.url,
        "sqlite://.runtime/data/codex-proxy-rs.sqlite"
    );
    assert_eq!(cfg.security.master_key_file, ".runtime/data/master.key");
    assert_eq!(
        cfg.security.api_key_pepper_file,
        ".runtime/data/api-key-pepper.key"
    );
    assert_eq!(cfg.logging.directory, ".runtime/logs");
}

#[test]
fn config_loader_merges_config_and_local_yaml() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.yaml"),
        r#"
server:
  host: 127.0.0.1
  port: 8080
api:
  base_url: https://chatgpt.com/backend-api
model:
  default: gpt-5.5
  default_reasoning_effort: null
  default_service_tier: null
  aliases: {}
auth:
  refresh_margin_seconds: 300
  refresh_enabled: true
  refresh_concurrency: 2
  max_concurrent_per_account: 3
  request_interval_ms: 50
  rotation_strategy: least_used
  tier_priority: []
  oauth_client_id: app_test_client
  oauth_auth_endpoint: https://auth.example.test/oauth/authorize
  oauth_token_endpoint: https://auth.example.test/oauth/token
quota:
  refresh_interval_minutes: 5
  warning_thresholds:
    primary:
      - 80
      - 90
    secondary:
      - 80
      - 90
  skip_exhausted: true
usage_stats:
  history_retention_days: null
database:
  url: sqlite://.runtime/data/codex-proxy-rs.sqlite
security:
  master_key_file: .runtime/data/master.key
  api_key_pepper_file: .runtime/data/api-key-pepper.key
tls:
  force_http11: false
ws_pool:
  enabled: true
  max_age_ms: 3300000
  max_per_account: 8
admin:
  session_ttl_minutes: 1440
logging:
  directory: .runtime/logs
  retention_days: 14
  enabled: false
  capacity: 2000
  capture_body: false
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("local.yaml"),
        r#"
server:
  host: 0.0.0.0
logging:
  directory: local-logs
ws_pool:
  enabled: false
  max_age_ms: 120000
  max_per_account: 2
"#,
    )
    .unwrap();

    let cfg = AppConfig::load_from_dir(dir.path()).unwrap();

    assert_eq!(cfg.server.host, "0.0.0.0");
    assert_eq!(cfg.server.port, 8080);
    assert_eq!(
        cfg.database.url,
        "sqlite://.runtime/data/codex-proxy-rs.sqlite"
    );
    assert_eq!(cfg.model.default_model, "gpt-5.5");
    assert_eq!(cfg.auth.max_concurrent_per_account, 3);
    assert_eq!(cfg.auth.oauth_client_id, "app_test_client");
    assert_eq!(
        cfg.auth.oauth_auth_endpoint,
        "https://auth.example.test/oauth/authorize"
    );
    assert_eq!(
        cfg.auth.oauth_token_endpoint,
        "https://auth.example.test/oauth/token"
    );
    assert!(!cfg.ws_pool.enabled);
    assert_eq!(cfg.ws_pool.max_age_ms, 120_000);
    assert_eq!(cfg.ws_pool.max_per_account, 2);
    assert_eq!(cfg.logging.directory, "local-logs");
    assert_eq!(cfg.logging.retention_days, 14);
}

#[test]
fn logging_config_rejects_unsupported_size_rotation_field() {
    let err = serde_yml::from_str::<LoggingConfig>(
        r#"
directory: .runtime/logs
max_file_bytes: 10485760
retention_days: 14
enabled: false
capacity: 2000
capture_body: false
"#,
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("max_file_bytes"),
        "expected max_file_bytes to be rejected, got {err}"
    );
}
