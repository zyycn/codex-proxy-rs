use axum::{
    Router,
    body::{Body, Bytes},
    extract::DefaultBodyLimit,
    http::{Method, Request, StatusCode},
    routing::post,
};
use gateway_api::openai::router::MAX_CLIENT_REQUEST_BODY_BYTES;
use tower::ServiceExt;

use super::api_router_with_origins;
use super::models::ModelsExecution;

fn body_limit_app() -> Router {
    Router::new()
        .route(
            "/v1/responses",
            post(|body: Bytes| async move {
                if body.len() == MAX_CLIENT_REQUEST_BODY_BYTES {
                    StatusCode::NO_CONTENT
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            }),
        )
        .layer(DefaultBodyLimit::max(MAX_CLIENT_REQUEST_BODY_BYTES))
}

#[tokio::test]
async fn responses_body_limit_should_accept_exactly_sixteen_mibibytes() {
    let response = body_limit_app()
        .oneshot(
            Request::post("/v1/responses")
                .body(Body::from(vec![b'a'; MAX_CLIENT_REQUEST_BODY_BYTES]))
                .expect("build exact-limit request"),
        )
        .await
        .expect("route exact-limit request");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn responses_body_limit_should_reject_one_byte_over_sixteen_mibibytes() {
    let response = body_limit_app()
        .oneshot(
            Request::post("/v1/responses")
                .body(Body::from(vec![b'a'; MAX_CLIENT_REQUEST_BODY_BYTES + 1]))
                .expect("build over-limit request"),
        )
        .await
        .expect("route over-limit request");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
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
