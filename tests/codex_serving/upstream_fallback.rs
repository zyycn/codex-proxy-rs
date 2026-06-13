use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{DateTime, Utc};
use secrecy::ExposeSecret;
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    codex::accounts::{cookies::repository::CookieRepository, repository::AccountRepository},
    codex::gateway::oauth::RefreshFailure,
};

use crate::support::{
    response_json, response_text,
    upstream::{
        build_imported_app_with_accounts, build_imported_app_with_refresher,
        build_imported_app_with_token_refresher, FailingTokenRefresher, ImportAccount,
    },
};

const AFTER_429_SSE: &str = include_str!("../fixtures/responses/http_sse/after_429.sse");
const STREAM_AFTER_429_SSE: &str =
    include_str!("../fixtures/responses/http_sse/stream_after_429.sse");
const AFTER_402_SSE: &str = include_str!("../fixtures/responses/http_sse/after_402.sse");
const AFTER_403_SSE: &str = include_str!("../fixtures/responses/http_sse/after_403.sse");
const AFTER_CLOUDFLARE_403_SSE: &str =
    include_str!("../fixtures/responses/http_sse/after_cloudflare_403.sse");
const AFTER_REFRESH_SSE: &str = include_str!("../fixtures/responses/http_sse/after_refresh.sse");
const STREAM_AFTER_REFRESH_SSE: &str =
    include_str!("../fixtures/responses/http_sse/stream_after_refresh.sse");

#[tokio::test]
async fn v1_responses_should_retry_next_account_after_429_retry_after() {
    let started_at = Utc::now();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "120")
                .set_body_json(json!({
                    "error": {
                        "message": "rate limited",
                        "resets_in_seconds": 120
                    }
                })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-b"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(AFTER_429_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;
    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_429");
    let usage_a: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_a")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(usage_a, (1, 0, 0));
    let usage_b: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_b")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(usage_b, (1, 5, 2));
    let cooldown: (i64, Option<String>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = ?",
    )
    .bind("acct_a")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(cooldown.0, 1);
    let cooldown_until = DateTime::parse_from_rfc3339(cooldown.1.as_deref().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    assert!(cooldown_until > started_at);
}

#[tokio::test]
async fn v1_responses_stream_should_retry_next_account_after_429_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "90")
                .set_body_json(json!({
                    "error": {"message": "rate limited"}
                })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-b"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(STREAM_AFTER_429_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;
    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
    assert!(body.contains("resp_stream_after_429"));
}

#[tokio::test]
async fn v1_responses_should_mark_quota_exhausted_after_402_and_retry_next_account() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "error": {"message": "quota exhausted"}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-b"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(AFTER_402_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_402");
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_a")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn v1_responses_should_mark_banned_after_403_and_retry_next_account() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"message": "account banned"}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-b"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(AFTER_403_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_403");
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_a")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "banned");
}

#[tokio::test]
async fn v1_responses_should_cool_down_cloudflare_403_and_retry_next_account() {
    let started_at = Utc::now();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<html><title>Just a moment...</title><body>cf_chl challenge</body></html>",
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-b"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(AFTER_CLOUDFLARE_403_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;
    let cookie_repo = CookieRepository::new(imported.pool.clone(), imported.secret_box.clone());
    cookie_repo
        .set_cookie_header("acct_a", "cf_clearance=old")
        .await
        .unwrap();
    cookie_repo
        .set_cookie_header("acct_b", "cf_clearance=keep")
        .await
        .unwrap();
    assert_eq!(
        cookie_repo
            .cookie_header("acct_a", "chatgpt.com")
            .await
            .unwrap()
            .as_deref(),
        Some("cf_clearance=old")
    );

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_cf");
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_a")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "active");
    let (cooldown_until,): (Option<String>,) =
        sqlx::query_as("select cloudflare_cooldown_until from accounts where id = ?")
            .bind("acct_a")
            .fetch_one(&imported.pool)
            .await
            .unwrap();
    let cooldown_until = DateTime::parse_from_rfc3339(cooldown_until.as_deref().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    assert!(cooldown_until > started_at);
    assert_eq!(
        cookie_repo
            .cookie_header("acct_a", "chatgpt.com")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        cookie_repo
            .cookie_header("acct_b", "chatgpt.com")
            .await
            .unwrap()
            .as_deref(),
        Some("cf_clearance=keep")
    );
}

#[tokio::test]
async fn v1_responses_should_refresh_after_401_and_retry_non_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked"}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer refreshed-access"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(AFTER_REFRESH_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_refresher(server.uri(), "refreshed-access").await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .header("x-request-id", "req_refresh_non_stream")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_refresh");
    let repo = AccountRepository::new(imported.pool.clone(), imported.secret_box);
    let account = repo.get("acct_imported").await.unwrap().unwrap();
    assert_eq!(account.access_token.expose_secret(), "refreshed-access");
    assert_eq!(
        account.refresh_token.unwrap().expose_secret(),
        "refresh-secret"
    );
}

#[tokio::test]
async fn v1_responses_should_refresh_after_401_and_retry_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked"}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer refreshed-stream-access"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(STREAM_AFTER_REFRESH_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_refresher(server.uri(), "refreshed-stream-access").await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .header("x-request-id", "req_refresh_stream")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
    assert!(body.contains("event: response.completed"));
    let usage: (i64, i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens, cached_tokens from account_usage where account_id = ?",
    )
    .bind("acct_imported")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(usage, (1, 4, 2, 0));
}

#[tokio::test]
async fn v1_responses_should_log_refresh_failure_after_401() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked"}
        })))
        .mount(&server)
        .await;
    let imported = build_imported_app_with_token_refresher(
        server.uri(),
        FailingTokenRefresher {
            failure: RefreshFailure::InvalidGrant,
        },
    )
    .await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .header("x-request-id", "req_refresh_failed")
                .body(Body::from(r#"{"model":"gpt-5.5","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_imported")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "expired");
    let refresh_event: (String, String, String) = sqlx::query_as(
        "select level, message, metadata_json from event_logs where request_id = ? and kind = 'account.refresh'",
    )
    .bind("req_refresh_failed")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    let metadata: Value = serde_json::from_str(&refresh_event.2).unwrap();
    assert_eq!(refresh_event.0, "warn");
    assert_eq!(refresh_event.1, "上游 401 后账户刷新失败");
    assert_eq!(metadata["failure"], "invalidGrant");
    assert_eq!(metadata["accountStatus"], "expired");
    assert_eq!(metadata["trigger"], "upstream_401");
}
