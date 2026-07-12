use super::*;
use axum::body::to_bytes;
use codex_proxy_rs::{
    models::{
        store::ModelSnapshotStore,
        types::{BackendModelEntry, ModelPlanSnapshot},
    },
    upstream::openai::protocol::sse::{encode_sse_event, parse_sse_events},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

async fn seed_test_account(pool: &PgPool) {
    seed_account(
        pool,
        NewAccount {
            id: "acct_test".to_string(),
            email: Some("test@example.com".to_string()),
            account_id: Some("chatgpt-test".to_string()),
            user_id: None,
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new("access-test".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
}

fn test_events(body: &str) -> Vec<Value> {
    parse_sse_events(body)
        .unwrap()
        .into_iter()
        .map(|event| serde_json::from_str::<Value>(&event.data).unwrap())
        .collect()
}

#[tokio::test]
async fn account_models_should_return_redis_snapshot_models() {
    let server = MockServer::start().await;
    let (app, state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-account-models",
        91,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    seed_test_account(&pool).await;
    seed_model_snapshot(&state, "plus").await;
    state
        .services
        .models
        .reload_from_store()
        .await
        .expect("model catalog should reload from seeded snapshot");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/models?id=acct_test")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_account_models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        requests.is_empty(),
        "model list should be loaded from Redis"
    );
    let body = response_json(response).await;
    assert_eq!(body["data"]["models"][0]["id"], "gpt-5.5");
    assert_eq!(body["data"]["models"][0]["label"], "GPT 5.5");
    assert_eq!(body["data"]["models"][1]["id"], "gpt-5.4");
    assert_eq!(body["data"]["models"][1]["label"], "GPT 5.4");
}

#[tokio::test]
async fn account_models_should_fetch_missing_plan_snapshot_with_account_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/codex/models"))
        .and(header("authorization", "Bearer access-plan-b"))
        .and(header("chatgpt-account-id", "chatgpt-plan-b"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                {
                    "slug": "gpt-plan-b-live",
                    "display_name": "GPT Plan B Live",
                    "description": "Live model",
                    "is_default": true,
                    "supported_reasoning_efforts": [{"reasoning_effort": "medium", "description": "medium"}],
                    "default_reasoning_effort": "medium",
                    "input_modalities": ["text"],
                    "output_modalities": ["text"],
                    "supports_personality": false
                }
            ]
        })))
        .mount(&server)
        .await;
    let (app, state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-account-models-unfetched-plan",
        93,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_plan_b".to_string(),
            email: Some("plan-b@example.com".to_string()),
            account_id: Some("chatgpt-plan-b".to_string()),
            user_id: None,
            label: None,
            plan_type: Some("plan-b".to_string()),
            access_token: SecretString::new("access-plan-b".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    seed_model_snapshot(&state, "plus").await;
    state
        .services
        .models
        .reload_from_store()
        .await
        .expect("model catalog should reload from seeded snapshot");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/models?id=acct_plan_b")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_account_models_unfetched_plan")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(requests.len(), 1, "missing plan should be fetched upstream");
    let body = response_json(response).await;
    assert_eq!(body["data"]["models"][0]["id"], "gpt-plan-b-live");
    assert_eq!(body["data"]["models"][0]["label"], "GPT Plan B Live");
    let model_snapshots =
        codex_proxy_rs::models::store::RedisModelSnapshotStore::new(state.redis.clone());
    let stored = model_snapshots.list_plan_snapshots().await.unwrap();
    assert!(stored.iter().any(|snapshot| snapshot.plan_type == "plan-b"));
}

#[tokio::test]
async fn account_models_should_return_models_for_account_plan_snapshot() {
    let server = MockServer::start().await;
    let (app, state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-account-models-matching-plan",
        94,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_plan_b_snapshot".to_string(),
            email: Some("plan-b-snapshot@example.com".to_string()),
            account_id: Some("chatgpt-plan-b-snapshot".to_string()),
            user_id: None,
            label: None,
            plan_type: Some("plan-b".to_string()),
            access_token: SecretString::new("access-plan-b-snapshot".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    seed_model_snapshot(&state, "plan-a").await;
    seed_single_model_snapshot(&state, "plan-b", "gpt-plan-b-only", "GPT Plan B Only").await;
    state
        .services
        .models
        .reload_from_store()
        .await
        .expect("model catalog should reload from seeded snapshots");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/models?id=acct_plan_b_snapshot")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_account_models_matching_plan")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        requests.is_empty(),
        "model list should be loaded from Redis"
    );
    let body = response_json(response).await;
    assert_eq!(body["data"]["models"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["models"][0]["id"], "gpt-plan-b-only");
    assert_eq!(body["data"]["models"][0]["label"], "GPT Plan B Only");
}

async fn seed_model_snapshot(state: &AdminAccountsTestState, plan_type: &str) {
    let snapshot = ModelPlanSnapshot::from_backend_entries(
        plan_type,
        vec![
            BackendModelEntry {
                id: Some("gpt-5.5".to_string()),
                name: Some("GPT 5.5".to_string()),
                ..BackendModelEntry::default()
            },
            BackendModelEntry {
                id: Some("gpt-5.4".to_string()),
                name: Some("GPT 5.4".to_string()),
                ..BackendModelEntry::default()
            },
        ],
    );
    codex_proxy_rs::models::store::RedisModelSnapshotStore::new(state.redis.clone())
        .replace_plan_snapshots(&[snapshot])
        .await
        .unwrap();
}

async fn seed_single_model_snapshot(
    state: &AdminAccountsTestState,
    plan_type: &str,
    model_id: &str,
    display_name: &str,
) {
    let snapshot = ModelPlanSnapshot::from_backend_entries(
        plan_type,
        vec![BackendModelEntry {
            id: Some(model_id.to_string()),
            name: Some(display_name.to_string()),
            ..BackendModelEntry::default()
        }],
    );
    codex_proxy_rs::models::store::RedisModelSnapshotStore::new(state.redis.clone())
        .replace_plan_snapshots(&[snapshot])
        .await
        .unwrap();
}

#[tokio::test]
async fn account_test_stream_should_translate_upstream_responses_sse() {
    let server = MockServer::start().await;
    let upstream_sse = [
        encode_sse_event(
            "",
            &json!({
                "type": "response.output_text.delta",
                "delta": "ok"
            })
            .to_string(),
        ),
        encode_sse_event("", &json!({ "type": "response.completed" }).to_string()),
    ]
    .join("");
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(upstream_sse),
        )
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-account-test-stream",
        92,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    seed_test_account(&pool).await;
    let store = codex_proxy_rs::fleet::store::PgAccountStore::new(pool.clone());
    store
        .set_status("acct_test", AccountStatus::QuotaExhausted)
        .await
        .expect("pre-test status should persist");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/test?id=acct_test&modelId=gpt-5.5")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_account_test_stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let events = test_events(&body);
    let requests = server.received_requests().await.unwrap();
    let upstream = requests
        .iter()
        .find(|request| request.url.path() == "/backend-api/codex/responses")
        .expect("test request should hit responses upstream");
    let upstream_body: Value = serde_json::from_slice(&upstream.body).unwrap();

    assert_eq!(events[0]["type"], "test_start");
    assert_eq!(events[0]["model"], "gpt-5.5");
    assert_eq!(events[1]["type"], "request");
    assert_eq!(events[1]["payload"]["model"], "gpt-5.5");
    assert_eq!(events[1]["payload"]["stream"], true);
    assert_eq!(events[1]["payload"]["store"], false);
    assert_eq!(events[2]["type"], "content");
    assert_eq!(events[2]["text"], "ok");
    assert_eq!(events[3]["type"], "test_complete");
    assert_eq!(events[3]["success"], true);
    assert_eq!(events[3]["accountStatus"], "active");
    assert_eq!(upstream_body["model"], "gpt-5.5");
    assert_eq!(upstream_body["stream"], true);
    assert_eq!(upstream_body["store"], false);
    assert_eq!(
        store
            .get("acct_test")
            .await
            .unwrap()
            .expect("account should exist")
            .status,
        AccountStatus::Active
    );
}

#[tokio::test]
async fn account_test_stream_should_preserve_manually_disabled_status_on_success() {
    let server = MockServer::start().await;
    let upstream_sse = encode_sse_event("", &json!({ "type": "response.completed" }).to_string());
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(upstream_sse),
        )
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-account-test-disabled-preserved",
        94,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    seed_test_account(&pool).await;
    let store = codex_proxy_rs::fleet::store::PgAccountStore::new(pool.clone());
    store
        .set_status("acct_test", AccountStatus::Disabled)
        .await
        .expect("pre-test status should persist");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/test?id=acct_test&modelId=gpt-5.5")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();

    let events = test_events(&body);
    assert_eq!(events.last().unwrap()["type"], "test_complete");
    assert_eq!(events.last().unwrap()["accountStatus"], "disabled");
    assert_eq!(
        store.get("acct_test").await.unwrap().unwrap().status,
        AccountStatus::Disabled
    );
}

#[tokio::test]
async fn account_test_stream_should_mark_expired_after_auth_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {
                "code": "token_expired",
                "message": "token expired"
            }
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-account-test-expired",
        95,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    seed_test_account(&pool).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/test?id=acct_test&modelId=gpt-5.5")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let events = test_events(&body);
    let store = codex_proxy_rs::fleet::store::PgAccountStore::new(pool);
    let stored = store.get("acct_test").await.unwrap().unwrap();

    assert_eq!(events.last().unwrap()["type"], "error");
    assert_eq!(events.last().unwrap()["accountStatus"], "expired");
    assert_eq!(stored.status, AccountStatus::Expired);
    assert!(stored.next_refresh_at.is_none());
}

#[tokio::test]
async fn account_test_stream_should_mark_quota_exhausted_after_failed_sse() {
    let server = MockServer::start().await;
    let upstream_sse = encode_sse_event(
        "",
        &json!({
            "type": "response.failed",
            "response": {
                "error": {
                    "code": "quota_exceeded",
                    "message": "quota exhausted"
                }
            }
        })
        .to_string(),
    );
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(upstream_sse),
        )
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-account-test-quota",
        96,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    seed_test_account(&pool).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/test?id=acct_test&modelId=gpt-5.5")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let events = test_events(&body);
    let store = codex_proxy_rs::fleet::store::PgAccountStore::new(pool);
    let stored = store.get("acct_test").await.unwrap().unwrap();

    assert_eq!(events.last().unwrap()["type"], "error");
    assert_eq!(events.last().unwrap()["accountStatus"], "quota_exhausted");
    assert_eq!(stored.status, AccountStatus::QuotaExhausted);
}
