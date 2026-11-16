//! 部署前缀校验、静态页面交付与发行文件替换后的行为

use std::fs;
use std::path::Path;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

#[test]
fn omitted_prefix_keeps_existing_configuration_compatible() {
    let config: gateway_api::ApiConfig = serde_json::from_value(serde_json::json!({
        "asset_directory": ".",
        "cors_allowed_origins": [],
        "request_timeout_seconds": null,
        "request_id_header": "x-request-id"
    }))
    .unwrap();
    assert_eq!(config.web_base_path, gateway_api::WebBasePath::default());
}

#[test]
fn deployment_prefix_accepts_only_same_origin_path_segments() {
    for value in ["/", "/cprs/", "/nested/cpr-v3.50_0~/"] {
        gateway_api::WebBasePath::try_from(value.to_owned()).expect(value);
    }
    for value in [
        "",
        "cprs/",
        "/cprs",
        "//",
        "/a//b/",
        "/../",
        "/./",
        "/%2e/",
        "/a?b/",
        "/a#b/",
        "/a\\b/",
        "/\"/",
        "/</",
        "https://example.com/",
    ] {
        assert!(
            gateway_api::WebBasePath::try_from(value.to_owned()).is_err(),
            "{value}"
        );
    }
}

#[tokio::test]
async fn deployment_prefix_survives_release_replacement_without_changing_files() {
    let directory = std::env::temp_dir().join(format!("cpr-web-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(directory.join("web/assets")).unwrap();
    fs::write(directory.join("web/assets/app.js"), "export default 1").unwrap();
    let original = "<head><base href=\"/\"></head><body>first release</body>";
    fs::write(directory.join("web/index.html"), original).unwrap();
    for prefix in ["/", "/cprs/", "/nested/cprs/"] {
        let router = router(&directory.join("web"), prefix).await;
        for uri in ["/", "/index.html", "/login", "/admin/accounts"] {
            let response = router.clone().oneshot(request("GET", uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["cache-control"], "no-cache");
            assert!(!response.headers().contains_key("last-modified"));
            let html = to_bytes(response.into_body(), 4096).await.unwrap();
            assert_eq!(
                html.as_ref(),
                original
                    .replace("href=\"/\"", &format!("href=\"{prefix}\""))
                    .as_bytes()
            );
        }
        assert_eq!(
            fs::read_to_string(directory.join("web/index.html")).unwrap(),
            original
        );
        let head = router.oneshot(request("HEAD", "/login")).await.unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert!(to_bytes(head.into_body(), 4096).await.unwrap().is_empty());
    }

    let router = router(&directory.join("web"), "/cprs/").await;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/assets/app.js")
                .header("range", "bytes=0-5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        to_bytes(response.into_body(), 4096).await.unwrap().as_ref(),
        b"export"
    );

    fs::create_dir(directory.join("incoming")).unwrap();
    let replacement = original.replace("first release", "second release");
    fs::write(directory.join("incoming/index.html"), &replacement).unwrap();
    fs::rename(directory.join("web"), directory.join("backup")).unwrap();
    fs::rename(directory.join("incoming"), directory.join("web")).unwrap();
    let response = router
        .clone()
        .oneshot(request("GET", "/admin/accounts"))
        .await
        .unwrap();
    let html = to_bytes(response.into_body(), 4096).await.unwrap();
    assert!(String::from_utf8_lossy(&html).contains("href=\"/cprs/\""));
    assert!(String::from_utf8_lossy(&html).contains("second release"));
    assert_eq!(
        fs::read_to_string(directory.join("web/index.html")).unwrap(),
        replacement
    );
    fs::rename(directory.join("web"), directory.join("updated")).unwrap();
    fs::rename(directory.join("backup"), directory.join("web")).unwrap();
    let response = router.oneshot(request("GET", "/login")).await.unwrap();
    assert!(
        String::from_utf8_lossy(&to_bytes(response.into_body(), 4096).await.unwrap())
            .contains("first release")
    );
    fs::remove_dir_all(directory).unwrap();
}

async fn router(directory: &Path, prefix: &str) -> axum::Router {
    let admin = crate::admin::AdminTestFixture::new().await;
    crate::openai::api_router_with_config(
        admin.services,
        gateway_api::ApiConfig {
            asset_directory: directory.to_owned(),
            web_base_path: prefix.to_owned().try_into().unwrap(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".to_owned(),
        },
    )
}

fn request(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}
