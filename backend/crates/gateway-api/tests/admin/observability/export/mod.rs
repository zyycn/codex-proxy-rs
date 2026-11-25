//! 诊断下载的权限、证据完整性、脱敏与部分失败合同

mod contract;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use gateway_admin::model::observability::UsageDetail;
use gateway_api::admin::observability;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::response::{ops_error_record, usage_record_with_account};
use crate::admin::{AdminTestFixture, AdminTestState};

async fn export(
    fixture: &AdminTestFixture,
    query: &str,
    authenticated: bool,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut request = Request::builder()
        .uri(format!("/api/admin/diagnostics/export{query}"))
        .header("x-request-id", "diagnostics-export-test");
    if authenticated {
        request = request.header(header::COOKIE, "cpr_session=valid-session");
    }
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 20 * 1024 * 1024)
        .await
        .unwrap();
    let file = if status == StatusCode::OK {
        assert_eq!(headers[header::CONTENT_TYPE], "application/zip");
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&body)).unwrap();
        assert_eq!(archive.len(), 3);
        let file: Value =
            serde_json::from_reader(archive.by_name("diagnostics.json").unwrap()).unwrap();
        for name in ["summary.txt", "README.txt"] {
            use std::io::Read as _;
            let mut text = String::new();
            archive
                .by_name(name)
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            assert!(!text.is_empty());
            for marker in [
                "fixture-authorization",
                "fixture-refresh",
                "fixture-password",
                "fixture@example.invalid",
            ] {
                assert!(!text.contains(marker), "archive leaked {marker} in {name}");
            }
        }
        file
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, headers, file)
}
