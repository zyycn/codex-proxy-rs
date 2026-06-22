//! OpenAI 调试诊断工具。

use axum::http::HeaderMap;

fn forwarded_header_is_local(headers: &HeaderMap, name: &str) -> bool {
    let Some(value) = headers.get(name).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    value.split(',').next().is_some_and(is_local_host)
}

fn is_local_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

pub fn is_local_debug_request(headers: &HeaderMap) -> bool {
    forwarded_header_is_local(headers, "x-forwarded-for")
        && forwarded_header_is_local(headers, "x-real-ip")
}

pub fn local_debug_forbidden_response() -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse, Json};
    use serde_json::json;
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "message": "debug endpoints are only available from localhost",
                "type": "forbidden"
            }
        })),
    )
        .into_response()
}
