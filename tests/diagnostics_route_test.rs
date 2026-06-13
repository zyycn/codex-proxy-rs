use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

mod common;

use common::{response_json, upstream::build_imported_app};

#[tokio::test]
async fn debug_diagnostics_should_return_local_runtime_pool_transport_and_path_summary() {
    let imported = build_imported_app("https://chatgpt.test/backend-api".to_string()).await;

    let response = imported
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/diagnostics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["runtime"]["packageVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["paths"]["config"], "config.yaml");
    assert_eq!(body["paths"]["localConfig"], "local.yaml");
    assert!(body["paths"]["databaseUrl"]
        .as_str()
        .unwrap()
        .starts_with("sqlite://"));
    assert_eq!(
        body["transport"]["backendBaseUrl"],
        "https://chatgpt.test/backend-api"
    );
    assert_eq!(body["transport"]["tls"]["forceHttp11"], false);
    assert_eq!(
        body["transport"]["fingerprint"]["originator"],
        "Codex Desktop"
    );
    assert_eq!(body["accounts"]["repositoryAvailable"], true);
    assert_eq!(body["accounts"]["pool"]["total"], 1);
    assert_eq!(body["accounts"]["pool"]["active"], 1);
    assert_eq!(body["accounts"]["capacity"]["maxConcurrentPerAccount"], 3);
    assert_eq!(body["accounts"]["capacity"]["totalSlots"], 3);
    assert_eq!(body["accounts"]["capacity"]["availableSlots"], 3);
    assert_eq!(body["settings"]["defaultModel"], "gpt-5.5");

    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("access-secret"));
    assert!(!serialized.contains("refresh-secret"));
}

#[tokio::test]
async fn debug_diagnostics_should_reject_forwarded_remote_requests() {
    let imported = build_imported_app("https://chatgpt.test/backend-api".to_string()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/diagnostics")
                .header("x-forwarded-for", "203.0.113.10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn debug_fingerprint_should_return_local_static_fingerprint_summary() {
    let imported = build_imported_app("https://chatgpt.test/backend-api".to_string()).await;

    let response = imported
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/fingerprint")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["source"], "staticDefault");
    assert_eq!(body["originator"], "Codex Desktop");
    assert_eq!(body["appVersion"], "26.519.81530");
    assert_eq!(body["buildNumber"], "3178");
    assert_eq!(
        body["userAgent"],
        "Codex/26.519.81530 (darwin; arm64) Chromium/146"
    );
}

#[tokio::test]
async fn debug_fingerprint_should_reject_forwarded_remote_requests() {
    let imported = build_imported_app("https://chatgpt.test/backend-api".to_string()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/fingerprint")
                .header("x-real-ip", "203.0.113.20")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
