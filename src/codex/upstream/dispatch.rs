use axum::{http::StatusCode, Json};
use reqwest::Url;
use serde_json::Value;

use crate::codex::protocol::codex_to_openai::openai_error;

pub(crate) fn no_available_accounts_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(openai_error(
            "No available Codex accounts",
            "no_available_accounts",
        )),
    )
}

pub(crate) fn normalize_service_tier_for_upstream(service_tier: String) -> String {
    if service_tier == "fast" {
        "priority".to_string()
    } else {
        service_tier
    }
}

pub(super) fn request_domain(base_url: &str) -> Option<String> {
    Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(ToString::to_string))
}
