use std::time::Instant;

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
};
use chrono::{Duration, Utc};
use futures::{SinkExt, StreamExt};
use secrecy::SecretString;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request as WsRequest, Response as WsResponse},
        Message,
    },
};

use codex_proxy_rs::{
    codex::accounts::{
        model::AccountStatus,
        repository::{NewAccount, UsageDelta},
    },
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    platform::crypto::SecretBox,
    platform::storage::db::connect_sqlite,
    runtime::state::AppState,
};

use crate::support::response_text;

fn test_config(database_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig {
            base_url: "https://chatgpt.com/backend-api".to_string(),
        },
        model: ModelConfig {
            default_model: "gpt-5.5".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            aliases: Default::default(),
        },
        auth: AuthConfig {
            refresh_margin_seconds: 300,
            refresh_enabled: true,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "least_used".to_string(),
            tier_priority: Vec::new(),
            oauth_client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            oauth_auth_endpoint: "https://auth.openai.com/oauth/authorize".to_string(),
            oauth_token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
        },
        quota: QuotaConfig {
            refresh_interval_minutes: 5,
            warning_thresholds: QuotaWarningThresholds {
                primary: vec![80, 90],
                secondary: vec![80, 90],
            },
            skip_exhausted: true,
        },
        usage_stats: UsageStatsConfig {
            history_retention_days: None,
        },
        database: DatabaseConfig { url: database_url },
        security: SecurityConfig {
            master_key_file: "data/master.key".to_string(),
            api_key_pepper_file: "data/api-key-pepper.key".to_string(),
        },
        tls: TlsConfig {
            force_http11: false,
        },
        ws_pool: Default::default(),
        admin: AdminConfig {
            session_ttl_minutes: 1440,
            default_username: "admin".to_string(),
            default_password: "admin".to_string(),
            session_cleanup_interval_secs: 3600,
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}

#[tokio::test]
async fn app_state_should_restore_account_pool_from_sqlite_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("startup-pool.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([31u8; 32]);
    let repo = codex_proxy_rs::codex::accounts::repository::AccountRepository::new(
        pool.clone(),
        secret_box.clone(),
    );
    repo.insert(NewAccount {
        id: "acct_restored".to_string(),
        email: Some("user@example.com".to_string()),
        account_id: Some("chatgpt-account".to_string()),
        user_id: None,
        label: Some("primary".to_string()),
        plan_type: Some("plus".to_string()),
        access_token: SecretString::new("access-secret".to_string().into()),
        refresh_token: Some(SecretString::new("refresh-secret".to_string().into())),
        access_token_expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
    repo.record_usage(
        "acct_restored",
        UsageDelta {
            input_tokens: 10,
            output_tokens: 3,
            cached_tokens: 2,
            empty_response_count: 0,
            ..UsageDelta::default()
        },
    )
    .await
    .unwrap();
    let state = AppState::with_pool_and_secret_box(test_config(url), pool, secret_box);

    let restored = state.reload_account_pool_from_repository().await.unwrap();

    assert_eq!(restored, 1);
    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.id, "acct_restored");
    assert_eq!(acquired.access_token, "access-secret");
    assert!(acquired.last_used_at.is_some());
}

#[tokio::test]
async fn app_state_should_restore_persisted_cooldowns_into_runtime_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("startup-pool-cooldown.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([32u8; 32]);
    let repo = codex_proxy_rs::codex::accounts::repository::AccountRepository::new(
        pool.clone(),
        secret_box.clone(),
    );
    repo.insert(NewAccount {
        id: "acct_cooling".to_string(),
        email: Some("cooling@example.com".to_string()),
        account_id: Some("chatgpt-cooling".to_string()),
        user_id: None,
        label: None,
        plan_type: Some("plus".to_string()),
        access_token: SecretString::new("access-cooling".to_string().into()),
        refresh_token: Some(SecretString::new("refresh-cooling".to_string().into())),
        access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
    let cooldown_until = (Utc::now() + Duration::minutes(10)).to_rfc3339();
    sqlx::query(
        "update accounts set quota_limit_reached = 1, quota_cooldown_until = ?, cloudflare_cooldown_until = ? where id = ?",
    )
    .bind(&cooldown_until)
    .bind(&cooldown_until)
    .bind("acct_cooling")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_and_secret_box(test_config(url), pool, secret_box);

    let restored = state.reload_account_pool_from_repository().await.unwrap();

    assert_eq!(restored, 1);
    assert!(state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .is_none());
}

#[tokio::test]
async fn app_state_should_restore_session_affinity_from_sqlite() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        accept_successful_websocket_response(
            &listener,
            "Bearer access-a",
            Some("turn_restore"),
            "resp_restore_second",
        )
        .await
    });
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("startup-affinity.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([33u8; 32]);
    let repo = codex_proxy_rs::codex::accounts::repository::AccountRepository::new(
        pool.clone(),
        secret_box.clone(),
    );
    repo.insert(NewAccount {
        id: "acct_a".to_string(),
        email: Some("a@example.com".to_string()),
        account_id: Some("chatgpt-a".to_string()),
        user_id: None,
        label: None,
        plan_type: Some("plus".to_string()),
        access_token: SecretString::new("access-a".to_string().into()),
        refresh_token: Some(SecretString::new("refresh-a".to_string().into())),
        access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
    repo.insert(NewAccount {
        id: "acct_b".to_string(),
        email: Some("b@example.com".to_string()),
        account_id: Some("chatgpt-b".to_string()),
        user_id: None,
        label: None,
        plan_type: Some("plus".to_string()),
        access_token: SecretString::new("access-b".to_string().into()),
        refresh_token: Some(SecretString::new("refresh-b".to_string().into())),
        access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
    repo.record_usage(
        "acct_a",
        UsageDelta {
            input_tokens: 1,
            output_tokens: 0,
            cached_tokens: 0,
            empty_response_count: 0,
            ..UsageDelta::default()
        },
    )
    .await
    .unwrap();
    let now = Utc::now();
    sqlx::query(
        "insert into session_affinities (response_id, account_id, conversation_id, turn_state, instructions_hash, input_tokens, function_call_ids_json, variant_hash, expires_at, created_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("resp_restore_first")
    .bind("acct_a")
    .bind("conv_restore")
    .bind("turn_restore")
    .bind(Option::<String>::None)
    .bind(3_i64)
    .bind(r#"["call_restore"]"#)
    .bind(Option::<String>::None)
    .bind((now + Duration::hours(3)).to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(&pool)
    .await
    .unwrap();
    let mut config = test_config(url);
    config.api.base_url = format!("http://{addr}");
    let state = AppState::with_pool_and_secret_box(config, pool, secret_box);
    state.reload_account_pool_from_repository().await.unwrap();
    assert_eq!(
        state
            .reload_session_affinity_from_repository()
            .await
            .unwrap(),
        1
    );

    let response = state
        .services
        .responses
        .handle(
            "req_restore_affinity",
            HeaderMap::new(),
            Bytes::from(
                r#"{"model":"gpt-5.5","input":[],"previous_response_id":"resp_restore_first"}"#,
            ),
            Instant::now(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("\"id\":\"resp_restore_second\""));
    let upstream_request = server.await.unwrap();
    assert_eq!(
        upstream_request["previous_response_id"],
        "resp_restore_first"
    );
}

#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn accept_successful_websocket_response(
    listener: &TcpListener,
    expected_authorization: &str,
    expected_turn_state: Option<&str>,
    response_id: &str,
) -> Value {
    let (stream, _) = listener.accept().await.unwrap();
    let expected_authorization = expected_authorization.to_string();
    let expected_turn_state = expected_turn_state.map(ToString::to_string);
    let mut websocket =
        accept_hdr_async(stream, move |request: &WsRequest, response: WsResponse| {
            assert_eq!(
                request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some(expected_authorization.as_str())
            );
            assert_eq!(
                request
                    .headers()
                    .get("x-codex-turn-state")
                    .and_then(|value| value.to_str().ok()),
                expected_turn_state.as_deref()
            );
            Ok(response)
        })
        .await
        .unwrap();
    let message = websocket.next().await.unwrap().unwrap();
    let request = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
    websocket
        .send(Message::Text(
            json!({
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "object": "response",
                    "status": "completed",
                    "output": [],
                    "usage": {
                        "input_tokens": 1,
                        "output_tokens": 1,
                        "total_tokens": 2,
                        "input_tokens_details": {"cached_tokens": 0}
                    }
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    websocket.close(None).await.unwrap();
    request
}
