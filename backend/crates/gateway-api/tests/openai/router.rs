use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use tower::ServiceExt;

use super::api_router_with_origins;
use super::models::ModelsExecution;

const REMOVED_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;

#[tokio::test]
async fn responses_body_should_accept_payload_above_the_removed_private_limit() {
    let router = api_router_with_origins(ModelsExecution::new(), Vec::new()).await;
    let response = router
        .oneshot(
            Request::post("/v1/responses")
                .body(Body::from(vec![b'a'; REMOVED_BODY_LIMIT_BYTES + 1]))
                .expect("build request above the removed limit"),
        )
        .await
        .expect("route request above the removed limit");

    // The handler sees the body and rejects the missing credentials. A restored body limit
    // would return 413 before authentication runs.
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn configured_cors_origin_assembles_router_and_answers_preflight() {
    let router = api_router_with_origins(
        ModelsExecution::new(),
        vec!["https://app.example.com".into()],
    )
    .await;

    let response = router
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/models")
                .header("origin", "https://app.example.com")
                .header("access-control-request-method", "GET")
                .header("access-control-request-headers", "authorization")
                .body(Body::empty())
                .expect("build preflight request"),
        )
        .await
        .expect("route preflight request");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("https://app.example.com")
    );
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-credentials")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    let allow_headers = response
        .headers()
        .get("access-control-allow-headers")
        .and_then(|value| value.to_str().ok())
        .expect("allow-headers present");
    assert!(allow_headers.contains("authorization"));
    assert!(allow_headers.contains("x-api-key"));
    assert!(allow_headers.contains("x-request-id"));
}
