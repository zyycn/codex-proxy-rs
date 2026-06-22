use axum::http::{header, HeaderMap, HeaderValue};

const SPA_CACHE_CONTROL: &str = "no-cache";
const ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const CONTENT_SECURITY_POLICY: &str = concat!(
    "default-src 'self'; ",
    "script-src 'self'; ",
    "style-src 'self' 'unsafe-inline'; ",
    "img-src 'self' data:; ",
    "font-src 'self' data:; ",
    "connect-src 'self'; ",
    "base-uri 'self'; ",
    "frame-ancestors 'none'"
);

pub fn cache_control_for_path(path: &str) -> &'static str {
    if path.starts_with("/assets/") {
        ASSET_CACHE_CONTROL
    } else {
        SPA_CACHE_CONTROL
    }
}

pub fn apply_static_headers(headers: &mut HeaderMap, request_path: &str) {
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control_for_path(request_path)),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
}

pub fn content_type_for_path(path: &str) -> HeaderValue {
    if path.ends_with(".html") || path == "/" {
        HeaderValue::from_static("text/html; charset=utf-8")
    } else if path.ends_with(".js") {
        HeaderValue::from_static("text/javascript; charset=utf-8")
    } else if path.ends_with(".css") {
        HeaderValue::from_static("text/css; charset=utf-8")
    } else if path.ends_with(".json") {
        HeaderValue::from_static("application/json")
    } else if path.ends_with(".svg") {
        HeaderValue::from_static("image/svg+xml")
    } else {
        HeaderValue::from_static("application/octet-stream")
    }
}
