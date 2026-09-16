mod admin;
mod architecture;
mod auth;
mod health;
mod key_usage;
mod openai;
mod support;

#[tokio::test]
async fn api_should_serve_resolved_assets_with_relative_environment_override() {
    use axum::{body::Body, http::Request};
    use std::{fs, process::Command};
    use tower::ServiceExt as _;

    const CHILD_ENV: &str = "CPR_TEST_ASSET_DIRECTORY";
    let Ok(directory) = std::env::var(CHILD_ENV) else {
        let directory = std::env::temp_dir().join(format!("cpr-assets-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(directory.join("deploy")).expect("config directory");
        fs::create_dir_all(directory.join("web/dist")).expect("asset directory");
        fs::write(directory.join("web/dist/index.html"), "resolved-web-assets").expect("index");
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "api_should_serve_resolved_assets_with_relative_environment_override",
            ])
            .current_dir(&directory)
            .env(CHILD_ENV, &directory)
            .env("CPR_WEB_DIST_DIR", "../web/dist")
            .output()
            .expect("isolated asset directory test");
        fs::remove_dir_all(directory).expect("remove fixture");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let mut config = gateway_api::ApiConfig {
        asset_directory: "unused".into(),
        cors_allowed_origins: Vec::new(),
        request_timeout_seconds: None,
        request_id_header: "x-request-id".to_owned(),
    };
    config
        .resolve_and_validate(&std::path::Path::new(&directory).join("deploy"))
        .expect("resolved API config");
    let admin = admin::AdminTestFixture::new().await;
    let response = openai::api_router_with_config(admin.services, config)
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .expect("static response");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), b"resolved-web-assets");
}
