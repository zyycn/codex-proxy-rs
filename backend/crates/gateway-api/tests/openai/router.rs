use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header::AUTHORIZATION},
};
use tower::ServiceExt;

use super::api_router_with_origins;
use super::models::ModelsExecution;

const REMOVED_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;

#[tokio::test]
async fn browser_origin_controls_http_admin_sessions_without_configuration() {
    use axum::body::to_bytes;
    use serde_json::{Value, json};
    for (origins, secure) in [
        (vec!["http://admin.example.test"], false),
        (vec!["http://192.0.2.1:8080"], false),
        (vec!["http://[::1]:8080"], false),
        (vec!["https://admin.example.test"], true),
        (vec![], true),
        (vec!["null"], true),
        (vec!["invalid-origin"], true),
        (vec!["http://admin.example.test/path"], true),
        (vec!["http://admin.example.test?https"], true),
        (vec!["http://user@admin.example.test"], true),
        (
            vec!["http://admin.example.test", "https://admin.example.test"],
            true,
        ),
    ] {
        let app = api_router_with_origins(ModelsExecution::new(), Vec::new()).await;
        let request = |path: &str| {
            let mut builder = Request::post(path)
                .header("content-type", "application/json")
                .header("x-forwarded-proto", "http")
                .header("forwarded", "proto=http");
            for origin in &origins {
                builder = builder.header("origin", *origin);
            }
            builder
        };
        let response = app
            .clone()
            .oneshot(
                request("/api/admin/auth/login")
                    .body(Body::from(
                        json!({"username": "admin_1", "password": "strong-admin-password"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()["set-cookie"].to_str().unwrap();
        let session = cookie.split(';').next().unwrap().to_owned();
        let attrs: Vec<_> = cookie.split(';').map(str::trim).collect();
        assert_eq!(attrs.contains(&"Secure"), secure);
        for attribute in ["Path=/", "HttpOnly", "SameSite=Lax"] {
            assert!(attrs.contains(&attribute));
        }

        let response = app
            .clone()
            .oneshot(
                Request::get("/api/admin/auth/status")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["data"]["authenticated"], true);

        let response = app
            .clone()
            .oneshot(
                request("/api/admin/auth/logout")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()["set-cookie"].to_str().unwrap();
        assert!(cookie.starts_with("cpr_admin_session=;"));
        let attrs: Vec<_> = cookie.split(';').map(str::trim).collect();
        assert_eq!(attrs.contains(&"Secure"), secure);
        for attribute in ["Path=/", "HttpOnly", "SameSite=Lax", "Max-Age=0"] {
            assert!(attrs.contains(&attribute));
        }

        let response = app
            .oneshot(
                Request::get("/api/admin/auth/status")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["data"]["authenticated"], false);
    }
}

#[tokio::test]
async fn removed_responses_review_route_should_not_reach_the_responses_handler() {
    let response = api_router_with_origins(ModelsExecution::new(), Vec::new())
        .await
        .oneshot(
            Request::post("/v1/responses/review")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .body(Body::empty())
                .expect("build removed review request"),
        )
        .await
        .expect("route removed review request");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn removed_models_catalog_extension_should_use_standard_model_lookup() {
    let response = api_router_with_origins(ModelsExecution::new(), Vec::new())
        .await
        .oneshot(
            Request::get("/v1/models/catalog")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .body(Body::empty())
                .expect("build removed model catalog request"),
        )
        .await
        .expect("route removed model catalog request");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn removed_model_info_route_should_return_not_found() {
    let response = api_router_with_origins(ModelsExecution::new(), Vec::new())
        .await
        .oneshot(
            Request::get("/v1/models/model-a/info")
                .header(AUTHORIZATION, "Bearer sk_models_test")
                .body(Body::empty())
                .expect("build removed model info request"),
        )
        .await
        .expect("route removed model info request");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

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
