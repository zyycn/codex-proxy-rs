use super::*;
use axum::body::to_bytes;
use codex_proxy_rs::upstream::protocol::sse::{encode_sse_event, parse_sse_events};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn seed_test_account(pool: &SqlitePool) {
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
async fn account_models_should_return_database_snapshot_models() {
    let server = MockServer::start().await;
    let (app, state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-account-models.sqlite",
        91,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    seed_test_account(&pool).await;
    seed_model_snapshot(&pool, "plus").await;
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
    assert!(requests.is_empty(), "model list should be loaded from DB");
    let body = response_json(response).await;
    assert_eq!(body["data"]["models"][0]["id"], "gpt-5.5");
    assert_eq!(body["data"]["models"][0]["label"], "GPT 5.5");
    assert_eq!(body["data"]["models"][1]["id"], "gpt-5.4");
    assert_eq!(body["data"]["models"][1]["label"], "GPT 5.4");
}

async fn seed_model_snapshot(pool: &SqlitePool, plan_type: &str) {
    let models_json = json!([
        {
            "id": "gpt-5.5",
            "displayName": "GPT 5.5",
            "description": "Test model",
            "isDefault": false,
            "supportedReasoningEfforts": [{"reasoningEffort": "medium", "description": "medium"}],
            "defaultReasoningEffort": "medium",
            "inputModalities": ["text"],
            "outputModalities": ["text"],
            "supportsPersonality": false,
            "upgrade": null,
            "source": "test"
        },
        {
            "id": "gpt-5.4",
            "displayName": "GPT 5.4",
            "description": "Test model",
            "isDefault": false,
            "supportedReasoningEfforts": [{"reasoningEffort": "medium", "description": "medium"}],
            "defaultReasoningEffort": "medium",
            "inputModalities": ["text"],
            "outputModalities": ["text"],
            "supportsPersonality": false,
            "upgrade": null,
            "source": "test"
        }
    ])
    .to_string();
    sqlx::query(
        "insert or replace into model_plan_snapshots (plan_type, models_json, fetched_at) values (?, ?, ?)",
    )
    .bind(plan_type)
    .bind(models_json)
    .bind("2026-06-29T00:00:00Z")
    .execute(pool)
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
        "admin-account-test-stream.sqlite",
        92,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    seed_test_account(&pool).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/test?id=acct_test")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_account_test_stream")
                .body(Body::from(json!({ "modelId": "gpt-5.5" }).to_string()))
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
    assert_eq!(upstream_body["model"], "gpt-5.5");
    assert_eq!(upstream_body["stream"], true);
    assert_eq!(upstream_body["store"], false);
}
