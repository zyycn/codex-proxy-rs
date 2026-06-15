use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{DateTime, Utc};
use serde_json::json;
use tower::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::codex::accounts::cookies::repository::CookieRepository;

use crate::support::{
    response_json, response_text,
    upstream::{build_imported_app_with_accounts, ImportAccount},
};

const AFTER_429_SSE: &str = include_str!("../fixtures/responses/http_sse/after_429.sse");
const STREAM_AFTER_429_SSE: &str =
    include_str!("../fixtures/responses/http_sse/stream_after_429.sse");
const AFTER_402_SSE: &str = include_str!("../fixtures/responses/http_sse/after_402.sse");
const AFTER_403_SSE: &str = include_str!("../fixtures/responses/http_sse/after_403.sse");
const AFTER_CLOUDFLARE_403_SSE: &str =
    include_str!("../fixtures/responses/http_sse/after_cloudflare_403.sse");
const AFTER_401_FALLBACK_SSE: &str =
    include_str!("../fixtures/responses/http_sse/after_401_fallback.sse");
const STREAM_AFTER_401_FALLBACK_SSE: &str =
    include_str!("../fixtures/responses/http_sse/stream_after_401_fallback.sse");
const SSE_RESPONSE_FAILED_QUOTA: &str =
    include_str!("../fixtures/responses/http_sse/response_failed_quota.sse");
const SSE_RESPONSE_FAILED_MODEL_UNSUPPORTED: &str =
    include_str!("../fixtures/responses/http_sse/response_failed_model_unsupported.sse");
const AFTER_5XX_RETRY_SSE: &str =
    include_str!("../fixtures/responses/http_sse/after_5xx_retry.sse");
const STREAM_AFTER_5XX_RETRY_SSE: &str =
    include_str!("../fixtures/responses/http_sse/stream_after_5xx_retry.sse");
const AFTER_MODEL_UNSUPPORTED_RETRY_SSE: &str =
    include_str!("../fixtures/responses/http_sse/after_model_unsupported_retry.sse");

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
async fn v1_responses_should_retry_same_account_after_5xx_before_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "temporary upstream failure"}
        })))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(AFTER_5XX_RETRY_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_5xx_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_5xx_b",
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
    assert_eq!(body["id"], "resp_after_5xx_retry");

    let requests = server.received_requests().await.unwrap();
    let access_a_posts = requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer access-a")
        })
        .count();
    let access_b_posts = requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer access-b")
        })
        .count();
    assert_eq!(access_a_posts, 3);
    assert_eq!(access_b_posts, 0);
}

#[tokio::test]
async fn v1_responses_stream_should_retry_same_account_after_5xx_before_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "temporary upstream failure"}
        })))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(STREAM_AFTER_5XX_RETRY_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_5xx_stream_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_5xx_stream_b",
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
    let body = response_text(response).await;
    assert!(body.contains("resp_stream_after_5xx_retry"));

    let requests = server.received_requests().await.unwrap();
    let access_a_posts = requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer access-a")
        })
        .count();
    let access_b_posts = requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer access-b")
        })
        .count();
    assert_eq!(access_a_posts, 3);
    assert_eq!(access_b_posts, 0);
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
async fn v1_responses_should_classify_non_stream_sse_failure_and_retry_next_account() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE_RESPONSE_FAILED_QUOTA),
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
                .set_body_string(AFTER_402_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_sse_failed_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_sse_failed_b",
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
    let first_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_sse_failed_a")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(first_status.0, "quota_exhausted");
}

#[tokio::test]
async fn v1_responses_should_retry_next_account_when_model_not_supported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "message": "Model gpt-5.5 is not supported on this account plan",
                "code": "model_not_supported"
            }
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
                .set_body_string(AFTER_MODEL_UNSUPPORTED_RETRY_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_model_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_model_b",
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
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_model_unsupported_retry");

    let requests = server.received_requests().await.unwrap();
    let access_a_posts = requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer access-a")
        })
        .count();
    let access_b_posts = requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer access-b")
        })
        .count();
    assert_eq!(access_a_posts, 1);
    assert_eq!(access_b_posts, 1);
}

#[tokio::test]
async fn v1_responses_should_classify_sse_model_not_supported_and_retry_next_account() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE_RESPONSE_FAILED_MODEL_UNSUPPORTED),
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
                .set_body_string(AFTER_MODEL_UNSUPPORTED_RETRY_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_sse_model_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_sse_model_b",
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
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_model_unsupported_retry");
}

#[tokio::test]
async fn v1_responses_should_retry_model_not_supported_only_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "message": "Model gpt-5.5 is not available on this account plan",
                "code": "model_not_available"
            }
        })))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(AFTER_MODEL_UNSUPPORTED_RETRY_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_model_once_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_model_once_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
            ImportAccount {
                id: "acct_model_once_c",
                account_id: "chatgpt-c",
                token: "access-c",
                refresh_token: "refresh-c",
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
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(
        body["error"]["message"],
        "Codex upstream error: {\"error\":{\"code\":\"model_not_available\",\"message\":\"Model gpt-5.5 is not available on this account plan\"}}"
    );

    let requests = server.received_requests().await.unwrap();
    let post_count = requests
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .count();
    assert_eq!(post_count, 2);
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
async fn v1_responses_should_return_502_when_cloudflare_challenge_has_no_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<html><title>Just a moment...</title><body>cf_chl challenge</body></html>",
        ))
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[ImportAccount {
            id: "acct_cf_single",
            account_id: "chatgpt-a",
            token: "access-a",
            refresh_token: "refresh-a",
        }],
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
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = response_json(response).await;
    assert_eq!(
        body["error"]["message"],
        "No accounts available. Upstream blocked the request (Cloudflare challenge)"
    );
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
async fn v1_responses_should_clear_cookies_and_retry_next_account_after_cloudflare_path_block_404()
{
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(404))
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
                id: "acct_cf_path_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_cf_path_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;
    let cookie_repo = CookieRepository::new(imported.pool.clone(), imported.secret_box.clone());
    cookie_repo
        .set_cookie_header("acct_cf_path_a", "cf_clearance=old")
        .await
        .unwrap();
    cookie_repo
        .set_cookie_header("acct_cf_path_b", "cf_clearance=keep")
        .await
        .unwrap();

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
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_cf");
    assert_eq!(
        cookie_repo
            .cookie_header("acct_cf_path_a", "chatgpt.com")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        cookie_repo
            .cookie_header("acct_cf_path_b", "chatgpt.com")
            .await
            .unwrap()
            .as_deref(),
        Some("cf_clearance=keep")
    );
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_cf_path_a")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "active");
}

#[tokio::test]
async fn v1_responses_should_disable_account_after_three_cloudflare_path_block_404s() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[ImportAccount {
            id: "acct_cf_path_disabled",
            account_id: "chatgpt-a",
            token: "access-a",
            refresh_token: "refresh-a",
        }],
    )
    .await;
    let cookie_repo = CookieRepository::new(imported.pool.clone(), imported.secret_box.clone());
    cookie_repo
        .set_cookie_header("acct_cf_path_disabled", "cf_clearance=old")
        .await
        .unwrap();

    for _ in 0..3 {
        let response = imported
            .app
            .clone()
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
                        r#"{"model":"gpt-5.5","input":[],"stream":false,"use_websocket":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    assert_eq!(
        cookie_repo
            .cookie_header("acct_cf_path_disabled", "chatgpt.com")
            .await
            .unwrap(),
        None
    );
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_cf_path_disabled")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "disabled");
}

#[tokio::test]
async fn v1_responses_should_mark_expired_after_401_and_retry_next_account_non_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked", "message": "token revoked"}
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
                .set_body_string(AFTER_401_FALLBACK_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_401_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_401_b",
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
                .header("x-request-id", "req_401_non_stream")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_401_fallback");
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_401_a")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "expired");
}

#[tokio::test]
async fn v1_responses_should_mark_expired_after_401_and_retry_next_account_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked", "message": "token revoked"}
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
                .set_body_string(STREAM_AFTER_401_FALLBACK_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_401_stream_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_401_stream_b",
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
                .header("x-request-id", "req_401_stream")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":false}"#,
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
    assert!(body.contains("resp_stream_after_401_fallback"));
    let usage: (i64, i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens, cached_tokens from account_usage where account_id = ?",
    )
    .bind("acct_401_stream_b")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(usage, (1, 4, 2, 0));
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_401_stream_a")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "expired");
}

#[tokio::test]
async fn v1_responses_should_mark_expired_and_return_401_when_401_has_no_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked", "message": "token revoked"}
        })))
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[ImportAccount {
            id: "acct_401_single",
            account_id: "chatgpt-a",
            token: "access-a",
            refresh_token: "refresh-a",
        }],
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
                .header("x-request-id", "req_401_no_fallback")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(
        body["error"]["message"],
        "All accounts exhausted (1 expired). Codex upstream error: {\"error\":{\"code\":\"token_revoked\",\"message\":\"token revoked\"}}"
    );
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_401_single")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "expired");
}

#[tokio::test]
async fn v1_responses_should_mark_banned_when_401_says_account_deactivated() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "account deactivated"}
        })))
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[ImportAccount {
            id: "acct_401_deactivated",
            account_id: "chatgpt-a",
            token: "access-a",
            refresh_token: "refresh-a",
        }],
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
                    r#"{"model":"gpt-5.5","input":[],"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_401_deactivated")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "banned");
}
