use codex_proxy_rs::config::AppConfig;
use std::fs;

#[test]
fn default_config_keeps_only_codex_backend() {
    let yaml = r#"
server:
  host: 127.0.0.1
  port: 8080
api:
  base_url: https://chatgpt.com/backend-api
auth:
  refresh_margin_seconds: 300
  refresh_enabled: true
  refresh_concurrency: 2
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
"#;
    let cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.api.base_url, "https://chatgpt.com/backend-api");
    assert_eq!(cfg.auth.refresh_margin_seconds, 300);
    assert_eq!(cfg.database.url, "sqlite://data/codex-proxy-rs.sqlite");
    assert_eq!(cfg.security.master_key_file, "data/master.key");
}

#[test]
fn config_loader_merges_default_local_and_cprs_env_overrides() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("default.yaml"),
        r#"
server:
  host: 127.0.0.1
  port: 8080
api:
  base_url: https://chatgpt.com/backend-api
auth:
  refresh_margin_seconds: 300
  refresh_enabled: true
  refresh_concurrency: 2
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

    let cfg = AppConfig::load_from_dir_with_env(
        dir.path(),
        [
            ("CPRS_PORT", "19090"),
            ("CPRS_DATABASE_URL", "sqlite://override.sqlite"),
            ("CPRS_LOG_MAX_FILE_BYTES", "4096"),
        ],
    )
    .unwrap();

    assert_eq!(cfg.server.host, "0.0.0.0");
    assert_eq!(cfg.server.port, 19090);
    assert_eq!(cfg.database.url, "sqlite://override.sqlite");
    assert_eq!(cfg.logging.directory, "local-logs");
    assert_eq!(cfg.logging.max_file_bytes, 4096);
}
