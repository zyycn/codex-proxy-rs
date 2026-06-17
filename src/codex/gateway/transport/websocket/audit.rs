use std::{
    io,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::codex::gateway::transport::types::CodexResponsesRequest;

use super::{
    codec::websocket_payload_audit_snapshot_from_request_body, http_sse_fallback_allowed,
    transport_for_request, CodexTransport, OpeningAuditSnapshot, PayloadAuditSnapshot,
};

pub const WS_AUDIT_DIR_ENV: &str = "CODEX_PROXY_WS_AUDIT_DIR";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WebSocketAuditArtifact {
    pub transport_mode: String,
    pub fallback_allowed: bool,
    pub opening: Option<OpeningAuditSnapshot>,
    pub payload: Option<PayloadAuditSnapshot>,
    pub error: Option<WebSocketAuditErrorSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WebSocketAuditErrorSnapshot {
    pub classification: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WebSocketParityDiff {
    pub differences: Vec<WebSocketParityDifference>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WebSocketParityDifference {
    pub path: String,
    pub current: Value,
    pub reference: Value,
}

impl WebSocketAuditArtifact {
    pub fn from_attempt(
        request: &CodexResponsesRequest,
        opening: OpeningAuditSnapshot,
        payload: PayloadAuditSnapshot,
    ) -> Self {
        Self {
            transport_mode: transport_mode_name(request),
            fallback_allowed: http_sse_fallback_allowed(request),
            opening: Some(opening),
            payload: Some(payload),
            error: None,
        }
    }
}

pub fn websocket_parity_diff(
    current: &WebSocketAuditArtifact,
    reference: &WebSocketAuditArtifact,
) -> WebSocketParityDiff {
    let mut differences = Vec::new();
    push_difference(
        &mut differences,
        "transport_mode",
        json!(current.transport_mode),
        json!(reference.transport_mode),
    );
    push_difference(
        &mut differences,
        "fallback_allowed",
        json!(current.fallback_allowed),
        json!(reference.fallback_allowed),
    );
    push_difference(
        &mut differences,
        "opening.request_line",
        opening_request_line(&current.opening),
        opening_request_line(&reference.opening),
    );
    push_difference(
        &mut differences,
        "opening.header_order",
        opening_header_order(&current.opening),
        opening_header_order(&reference.opening),
    );
    push_difference(
        &mut differences,
        "opening.sec_websocket_extensions",
        opening_header_value(&current.opening, "Sec-WebSocket-Extensions"),
        opening_header_value(&reference.opening, "Sec-WebSocket-Extensions"),
    );
    push_difference(
        &mut differences,
        "payload.top_level_keys",
        payload_top_level_keys(&current.payload),
        payload_top_level_keys(&reference.payload),
    );
    push_difference(
        &mut differences,
        "error.classification",
        error_classification(&current.error),
        error_classification(&reference.error),
    );
    WebSocketParityDiff { differences }
}

fn push_difference(
    differences: &mut Vec<WebSocketParityDifference>,
    path: &str,
    current: Value,
    reference: Value,
) {
    if current == reference {
        return;
    }
    differences.push(WebSocketParityDifference {
        path: path.to_string(),
        current,
        reference,
    });
}

fn opening_request_line(opening: &Option<OpeningAuditSnapshot>) -> Value {
    opening
        .as_ref()
        .map(|opening| json!(opening.request_line))
        .unwrap_or(Value::Null)
}

fn opening_header_order(opening: &Option<OpeningAuditSnapshot>) -> Value {
    opening
        .as_ref()
        .map(|opening| {
            json!(opening
                .headers
                .iter()
                .map(|header| header.name.as_str())
                .collect::<Vec<_>>())
        })
        .unwrap_or(Value::Null)
}

fn opening_header_value(opening: &Option<OpeningAuditSnapshot>, name: &str) -> Value {
    opening
        .as_ref()
        .and_then(|opening| {
            opening
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case(name))
        })
        .map(|header| json!(header.value))
        .unwrap_or(Value::Null)
}

fn payload_top_level_keys(payload: &Option<PayloadAuditSnapshot>) -> Value {
    payload
        .as_ref()
        .map(|payload| json!(payload.top_level_keys))
        .unwrap_or(Value::Null)
}

fn error_classification(error: &Option<WebSocketAuditErrorSnapshot>) -> Value {
    error
        .as_ref()
        .map(|error| json!(error.classification))
        .unwrap_or(Value::Null)
}

pub async fn record_websocket_audit_attempt_from_env(
    request: &CodexResponsesRequest,
    opening: &OpeningAuditSnapshot,
    payload: &Value,
) {
    let Some(dir) = websocket_audit_dir_from_env() else {
        return;
    };
    let artifact = WebSocketAuditArtifact::from_attempt(
        request,
        opening.clone(),
        websocket_payload_audit_snapshot_from_request_body(request, payload),
    );
    if let Err(error) = write_websocket_audit_artifact_for_dir(Some(&dir), &artifact).await {
        tracing::warn!(
            error = %error,
            "写入 Codex WebSocket audit artifact 失败"
        );
    }
}

pub async fn write_websocket_audit_artifact_for_dir(
    dir: Option<&Path>,
    artifact: &WebSocketAuditArtifact,
) -> io::Result<Option<PathBuf>> {
    let Some(dir) = dir.filter(|dir| !dir.as_os_str().is_empty()) else {
        return Ok(None);
    };

    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(audit_file_name());
    let body = serde_json::to_vec_pretty(artifact).map_err(io::Error::other)?;
    tokio::fs::write(&path, body).await?;
    Ok(Some(path))
}

fn websocket_audit_dir_from_env() -> Option<PathBuf> {
    std::env::var_os(WS_AUDIT_DIR_ENV)
        .filter(|value| !value.as_os_str().is_empty())
        .map(PathBuf::from)
}

fn audit_file_name() -> String {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    format!("codex-ws-audit-{timestamp}-{}.json", Uuid::new_v4())
}

fn transport_mode_name(request: &CodexResponsesRequest) -> String {
    match transport_for_request(request) {
        CodexTransport::HttpSse => "http_sse",
        CodexTransport::WebSocketPreferred => "websocket_preferred",
        CodexTransport::WebSocketRequired => "websocket_required",
    }
    .to_string()
}
