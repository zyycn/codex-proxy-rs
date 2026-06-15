#![allow(dead_code)]
// Each integration test crate imports this module independently, so a helper can be
// used by one route test and intentionally unused by another.

use axum::body::to_bytes;
use chrono::Utc;
use serde_json::Value;

pub mod admin_accounts;
pub mod upstream;

pub async fn seed_admin_session(pool: &sqlx::SqlitePool, session_id: &str) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
    )
    .bind("admin_1")
    .bind("hash")
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind("admin_1")
    .bind("2999-01-01T00:00:00Z")
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}

pub async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

pub async fn response_text(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

pub async fn response_sse_data(response: axum::response::Response) -> Vec<String> {
    response_text(response)
        .await
        .lines()
        .filter_map(|line| line.strip_prefix("data: ").map(ToString::to_string))
        .collect()
}

pub fn assert_response_failed_stream(
    body: &str,
    expected_type: &str,
    expected_code: &str,
    expected_message_parts: &[&str],
) {
    assert!(
        body.ends_with("\n\n"),
        "stream should end with an SSE boundary"
    );
    let mut event = None;
    let mut data = None;
    for line in body.lines() {
        if let Some(value) = line.strip_prefix("event: ") {
            event = Some(value);
        } else if let Some(value) = line.strip_prefix("data: ") {
            data = Some(value);
            break;
        }
    }

    assert_eq!(event, Some("response.failed"));
    let value: Value =
        serde_json::from_str(data.expect("response.failed stream should include data")).unwrap();
    assert_eq!(value["type"], "response.failed");
    let response = &value["response"];
    assert!(response["id"]
        .as_str()
        .is_some_and(|id| id.starts_with("resp_proxy_")));
    assert_eq!(response["status"], "failed");
    assert_eq!(response["error"], value["error"]);

    let error = &value["error"];
    assert_eq!(error["type"], expected_type);
    assert_eq!(error["code"], expected_code);
    let message = error["message"]
        .as_str()
        .expect("response.failed error should include message");
    for expected in expected_message_parts {
        assert!(
            message.contains(expected),
            "expected stream error message to contain {expected:?}, got {message:?}"
        );
    }
}
