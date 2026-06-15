use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint;

use crate::support::{
    response_json,
    upstream::{build_imported_app, build_imported_app_with_fingerprint},
};

fn diagnostics_fingerprint() -> Fingerprint {
    Fingerprint {
        originator: "Codex Desktop".to_string(),
        app_version: "27.100.200".to_string(),
        build_number: "9001".to_string(),
        platform: "linux".to_string(),
        arch: "x64".to_string(),
        chromium_version: "147".to_string(),
        user_agent_template: "Codex Desktop/{version} ({platform}; {arch})".to_string(),
        default_headers: Fingerprint::default_headers(),
        header_order: Fingerprint::default_header_order(),
    }
}

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
async fn debug_diagnostics_should_report_runtime_fingerprint() {
    let imported = build_imported_app_with_fingerprint(
        "https://chatgpt.test/backend-api".to_string(),
        diagnostics_fingerprint(),
    )
    .await;

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
    assert_eq!(body["transport"]["fingerprint"]["appVersion"], "27.100.200");
    assert_eq!(body["transport"]["fingerprint"]["buildNumber"], "9001");
    assert_eq!(body["transport"]["fingerprint"]["chromiumVersion"], "147");
    assert_eq!(
        body["transport"]["fingerprint"]["userAgent"],
        "Codex Desktop/27.100.200 (linux; x64)"
    );
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
async fn debug_fingerprint_should_return_runtime_fingerprint_summary() {
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
    assert_eq!(body["source"], "runtime");
    assert_eq!(body["originator"], "Codex Desktop");
    assert_eq!(body["appVersion"], "26.519.81530");
    assert_eq!(body["buildNumber"], "3178");
    assert_eq!(
        body["userAgent"],
        "Codex Desktop/26.519.81530 (darwin; arm64)"
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

#[tokio::test]
async fn debug_upstream_should_probe_with_runtime_fingerprint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {
                "message": "missing or invalid token"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let imported =
        build_imported_app_with_fingerprint(server.uri(), diagnostics_fingerprint()).await;

    let response = imported
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/upstream")
                .header("x-request-id", "req_debug_probe_fingerprint")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(body["statusCode"], 401);
    assert_eq!(requests.len(), 1);
    let headers = &requests[0].headers;
    assert_eq!(
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some("Codex Desktop/27.100.200 (linux; x64)")
    );
    assert_eq!(
        headers
            .get("sec-ch-ua")
            .and_then(|value| value.to_str().ok()),
        Some("\"Chromium\";v=\"147\", \"Not:A-Brand\";v=\"24\"")
    );
}

#[tokio::test]
async fn debug_upstream_should_probe_codex_models_endpoint_without_returning_secrets() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .and(header("originator", "Codex Desktop"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {
                "message": "missing or invalid token"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/upstream")
                .header("x-request-id", "req_debug_probe")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["target"], "codexModels");
    assert_eq!(body["backendBaseUrl"], server.uri());
    assert_eq!(body["reachable"], true);
    assert_eq!(body["statusCode"], 401);
    assert_eq!(body["authorization"], "rejected");
    assert!(body["endpoint"].as_str().unwrap().contains("/codex/models"));

    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("access-secret"));
    assert!(!serialized.contains("refresh-secret"));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].headers.get("x-client-request-id").is_none());
}

#[tokio::test]
async fn debug_upstream_should_reject_forwarded_remote_requests_without_probe() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/debug/upstream")
                .header("x-forwarded-for", "203.0.113.50")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_diagnostics_should_require_admin_session_cookie() {
    let imported = build_imported_app("https://chatgpt.test/backend-api".to_string()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/diagnostics")
                .header("x-request-id", "req_admin_diagnostics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_admin_diagnostics");
}

#[tokio::test]
async fn admin_diagnostics_should_return_admin_enveloped_runtime_summary_without_secrets() {
    let imported = build_imported_app("https://chatgpt.test/backend-api".to_string()).await;

    let response = imported
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/diagnostics")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_admin_diagnostics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["requestId"], "req_admin_diagnostics");
    assert_eq!(body["data"]["status"], "ok");
    assert_eq!(
        body["data"]["transport"]["backendBaseUrl"],
        "https://chatgpt.test/backend-api"
    );
    assert_eq!(body["data"]["accounts"]["pool"]["active"], 1);
    assert_eq!(body["data"]["accounts"]["capacity"]["totalSlots"], 3);

    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("access-secret"));
    assert!(!serialized.contains("refresh-secret"));
}
