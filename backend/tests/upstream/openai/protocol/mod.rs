use axum::http::HeaderMap;
use codex_proxy_rs::api::client::responses::build_codex_request;
use codex_proxy_rs::upstream::openai::protocol::events::{
    RateLimitWindow, TokenUsage, extract_sse_usage, extract_usage, parse_rate_limit_headers,
    parse_rate_limits_event, retry_after_seconds_from_body,
};
use codex_proxy_rs::upstream::openai::protocol::responses::{
    CodexResponsesRequest, PreviousResponseScope, TransportRequirement,
    response_body_has_semantic_output, response_event_signals, response_sse_event_is_terminal,
    transport_requirement,
};
use codex_proxy_rs::upstream::openai::protocol::sse::{
    SseEventDecoder, parse_sse_events, response_failed_sse_event,
};
use codex_proxy_rs::upstream::openai::protocol::websocket::{
    OpeningAuditHeader, OpeningAuditSnapshot, is_terminal_websocket_event,
    websocket_audit_artifact_from_attempt, websocket_event_to_sse_frame,
    websocket_metadata_turn_state, websocket_payload_audit_snapshot,
    websocket_response_completed_id,
};
use serde_json::json;

use crate::support::assertions::assert_substrings_appear_in_order;

mod events;
mod responses;
mod sse;
mod websocket;
