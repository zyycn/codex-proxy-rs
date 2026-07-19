use axum::{
    Router,
    body::{Body, Bytes},
    extract::DefaultBodyLimit,
    http::{Request, StatusCode},
    routing::post,
};
use gateway_api::openai::router::MAX_CLIENT_REQUEST_BODY_BYTES;
use tower::ServiceExt;

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
