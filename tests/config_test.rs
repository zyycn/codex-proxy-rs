use codex_proxy_rs::config::AppConfig;

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
