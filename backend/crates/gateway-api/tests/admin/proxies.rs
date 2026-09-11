use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use gateway_api::admin::proxies;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::{AdminTestFixture, AdminTestState};

const SESSION_COOKIE: &str = "cpr_admin_session=valid-session";

#[tokio::test]
async fn create_route_should_persist_and_return_masked_endpoint() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/proxies/create",
        Some(json!({
            "name": "HK 节点",
            "url": "http://user:secret@proxy.example.com:7890"
        })),
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let value = response_json(response).await;
    assert!(
        value["data"]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("pxy_") && id.len() == 36)
    );
    assert_eq!(value["data"]["record"]["name"], "HK 节点");
    assert_eq!(
        value["data"]["record"]["endpoint"],
        "http://proxy.example.com:7890/"
    );
    assert_eq!(value["data"]["record"]["accountCount"], 0);
    let record = value["data"]["record"].to_string();
    assert!(!record.contains("secret"), "credentials must never leak");
}

#[tokio::test]
async fn create_route_should_reject_invalid_proxy_url() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/proxies/create",
        Some(json!({ "name": "bad", "url": "ftp://proxy.example.com:21" })),
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_route_should_keep_camel_case_wire() {
    let fixture = authenticated_fixture().await;
    create_proxy(&fixture, "US 节点", "socks5://10.0.0.1:1080").await;
    let response = request(
        router(&fixture),
        Method::GET,
        "/api/admin/proxies?page=1&pageSize=20&search=us",
        None,
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let value = response_json(response).await;
    assert_eq!(value["data"]["page"]["page"], 1);
    assert_eq!(value["data"]["page"]["pageSize"], 20);
    assert_eq!(value["data"]["page"]["total"], 1);
    assert_eq!(value["data"]["items"][0]["name"], "US 节点");
    // socks5 是非 special scheme，URL 规范化不追加尾部斜杠。
    assert_eq!(
        value["data"]["items"][0]["endpoint"],
        "socks5://10.0.0.1:1080"
    );
    assert!(value["data"]["items"][0].get("account_count").is_none());
}

#[tokio::test]
async fn update_route_should_keep_url_when_omitted() {
    let fixture = authenticated_fixture().await;
    let id = create_proxy(&fixture, "JP 节点", "http://jp.example.com:8080").await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/proxies/update",
        Some(json!({ "id": id, "name": "JP 主力", "url": "" })),
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let value = response_json(response).await;
    assert_eq!(value["data"]["record"]["name"], "JP 主力");
    assert_eq!(
        value["data"]["record"]["endpoint"],
        "http://jp.example.com:8080/"
    );
}

#[tokio::test]
async fn reveal_route_should_return_full_url_once() {
    let fixture = authenticated_fixture().await;
    let id = create_proxy(&fixture, "SG 节点", "socks5h://user:pw@sg.example.com:1080").await;
    let response = request(
        router(&fixture),
        Method::GET,
        &format!("/api/admin/proxies/reveal?id={id}"),
        None,
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let value = response_json(response).await;
    assert_eq!(
        value["data"]["url"],
        "socks5h://user:pw@sg.example.com:1080"
    );
}

#[tokio::test]
async fn delete_route_should_remove_the_proxy() {
    let fixture = authenticated_fixture().await;
    let id = create_proxy(&fixture, "临时节点", "http://tmp.example.com:3128").await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/proxies/delete",
        Some(json!({ "id": id })),
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/proxies/delete",
        Some(json!({ "id": id })),
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_route_should_probe_saved_proxy_with_default_target() {
    let fixture = authenticated_fixture().await;
    let id = create_proxy(&fixture, "测试节点", "http://probe.example.com:8080").await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/proxies/test",
        Some(json!({ "id": id })),
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let value = response_json(response).await;
    assert_eq!(value["data"]["success"], true);
    assert_eq!(value["data"]["statusCode"], 200);
    assert_eq!(
        fixture
            .proxy_probe
            .last_target
            .lock()
            .expect("last target")
            .as_deref(),
        Some(gateway_admin::DEFAULT_PROXY_TEST_TARGET)
    );
}

#[tokio::test]
async fn test_route_should_probe_inline_proxy_with_custom_target() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/proxies/test",
        Some(json!({
            "url": "socks5://inline.example.com:1080",
            "targetUrl": "https://www.google.com/generate_204"
        })),
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let value = response_json(response).await;
    assert_eq!(value["data"]["success"], true);
    assert_eq!(
        value["data"]["targetUrl"],
        "https://www.google.com/generate_204"
    );
}

#[tokio::test]
async fn test_route_should_reject_missing_subject_and_bad_target() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/proxies/test",
        Some(json!({})),
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/proxies/test",
        Some(json!({
            "url": "http://inline.example.com:8080",
            "targetUrl": "file:///etc/passwd"
        })),
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn routes_should_require_an_admin_session() {
    let fixture = AdminTestFixture::new().await;
    let response = request(
        router(&fixture),
        Method::GET,
        "/api/admin/proxies",
        None,
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

async fn create_proxy(fixture: &AdminTestFixture, name: &str, url: &str) -> String {
    let response = request(
        router(fixture),
        Method::POST,
        "/api/admin/proxies/create",
        Some(json!({ "name": name, "url": url })),
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response_json(response).await["data"]["id"]
        .as_str()
        .expect("created proxy id")
        .to_owned()
}

async fn authenticated_fixture() -> AdminTestFixture {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    fixture
}

fn router(fixture: &AdminTestFixture) -> Router {
    proxies::router::<AdminTestState>().with_state(fixture.state())
}

async fn request(
    router: Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    authenticated: bool,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "req_proxies");
    if authenticated {
        builder = builder.header(header::COOKIE, SESSION_COOKIE);
    }
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).expect("serialize request body"))
        }
        None => Body::empty(),
    };
    router
        .oneshot(builder.body(body).expect("proxy request"))
        .await
        .expect("proxy response")
}

async fn response_json(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("proxy response body");
    serde_json::from_slice(&body).expect("proxy response JSON")
}
