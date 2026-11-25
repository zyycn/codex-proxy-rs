//! 管理端 JSON 压缩的协商、响应合同与重复编码回归测试

use std::io::Read as _;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use flate2::read::GzDecoder;
use tower::ServiceExt as _;
use tower_http::compression::CompressionLayer;

use super::AdminTestFixture;

async fn app() -> Router {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    crate::data_plane::api_router_with_admin(fixture.services)
}

async fn get(router: Router, uri: &str, encoding: Option<&str>) -> Response {
    let mut request = Request::builder()
        .uri(uri)
        .header(header::COOKIE, "cpr_session=valid-session")
        .header("x-request-id", "req_admin_compression");
    if let Some(encoding) = encoding {
        request = request.header(header::ACCEPT_ENCODING, encoding);
    }
    router
        .oneshot(request.body(Body::empty()).expect("request"))
        .await
        .expect("response")
}

async fn bytes(response: Response) -> Vec<u8> {
    to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("response body")
        .to_vec()
}

fn decode_gzip(body: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    GzDecoder::new(body)
        .read_to_end(&mut decoded)
        .expect("valid gzip body");
    decoded
}

#[tokio::test]
async fn gzip_should_preserve_admin_json_and_response_headers() {
    let router = app().await;
    let expected = bytes(get(router.clone(), "/api/admin/settings", None).await).await;
    assert!(expected.len() >= 1024);

    for encoding in ["gzip", "br, gzip;q=0.8, deflate;q=0.5", "*;q=0.5"] {
        let response = get(router.clone(), "/api/admin/settings", Some(encoding)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()["x-request-id"], "req_admin_compression");
        assert_eq!(response.headers()[header::VARY], "accept-encoding");
        assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
        let compressed = bytes(response).await;
        assert!(compressed.len() < expected.len());
        assert_eq!(decode_gzip(&compressed), expected, "{encoding}");
    }
}

#[tokio::test]
async fn clients_without_gzip_support_should_receive_original_json() {
    let router = app().await;
    let expected = bytes(get(router.clone(), "/api/admin/settings", None).await).await;
    for encoding in [None, Some("identity"), Some("br"), Some("gzip;q=0")] {
        let response = get(router.clone(), "/api/admin/settings", encoding).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(response.headers()[header::VARY], "accept-encoding");
        assert_eq!(bytes(response).await, expected, "{encoding:?}");
    }
}

#[tokio::test]
async fn small_admin_json_should_remain_uncompressed() {
    let router = app().await;
    let uri = "/api/admin/settings/admin-api-key";
    let expected = bytes(get(router.clone(), uri, None).await).await;
    assert!(expected.len() < 1024);
    let response = get(router, uri, Some("gzip")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
    assert_eq!(bytes(response).await, expected);
}

#[tokio::test]
async fn a_second_compression_layer_should_not_encode_gzip_again() {
    let router = app().await;
    let expected = bytes(get(router.clone(), "/api/admin/settings", None).await).await;
    let response = get(
        router.layer(CompressionLayer::new()),
        "/api/admin/settings",
        Some("gzip"),
    )
    .await;
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
    assert_eq!(response.headers()[header::VARY], "accept-encoding");
    assert_eq!(decode_gzip(&bytes(response).await), expected);
}
