use codex_proxy_rs::config::AppConfig;
use std::fs;

#[test]
fn default_config_keeps_only_codex_backend() {
    let cfg: AppConfig = serde_yml::from_str(include_str!("../config.yaml")).unwrap();
    assert_eq!(cfg.server.host, "0.0.0.0");
    assert_eq!(cfg.api.base_url, "https://chatgpt.com/backend-api");
    assert_eq!(cfg.model.default_model, "gpt-5.5");
    assert_eq!(cfg.auth.refresh_margin_seconds, 300);
    assert_eq!(cfg.auth.rotation_strategy, "least_used");
    assert_eq!(cfg.database.url, "sqlite://data/codex-proxy-rs.sqlite");
    assert_eq!(cfg.security.master_key_file, "data/master.key");
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
  url: sqlite://data/codex-proxy-rs.sqlite
security:
  master_key_file: data/master.key
  api_key_pepper_file: data/api-key-pepper.key
tls:
  force_http11: false
admin:
  session_ttl_minutes: 1440
logging:
  directory: logs
  max_file_bytes: 10485760
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
"#,
    )
    .unwrap();

    let cfg = AppConfig::load_from_dir(dir.path()).unwrap();

    assert_eq!(cfg.server.host, "0.0.0.0");
    assert_eq!(cfg.server.port, 8080);
    assert_eq!(cfg.database.url, "sqlite://data/codex-proxy-rs.sqlite");
    assert_eq!(cfg.model.default_model, "gpt-5.5");
    assert_eq!(cfg.auth.max_concurrent_per_account, 3);
    assert_eq!(cfg.logging.directory, "local-logs");
    assert_eq!(cfg.logging.max_file_bytes, 10485760);
}
