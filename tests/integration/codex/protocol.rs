use codex_proxy_rs::codex::models::{ModelConfig, ParsedModelName};
use codex_proxy_rs::codex::protocol::events::{
    cooldown_with_jitter, extract_sse_usage, extract_usage, parse_rate_limit_headers,
    parse_rate_limits_event, rate_limit_quota, retry_after_seconds_from_body, RateLimitWindow,
    TokenUsage,
};
use codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest;
use codex_proxy_rs::codex::protocol::sse::{parse_sse_events, sse_body_has_done, DONE_SSE_FRAME};
use codex_proxy_rs::codex::protocol::websocket::{
    classify_websocket_error_frame, is_terminal_websocket_event,
    retry_after_seconds_from_wrapped_error_headers,
    websocket_agent_message_output_item_event_invalid_required_fields,
    websocket_audit_artifact_from_attempt,
    websocket_compaction_output_item_event_invalid_required_fields,
    websocket_custom_tool_call_output_item_event_invalid_required_fields,
    websocket_custom_tool_call_output_payload_item_event_invalid_required_fields,
    websocket_delta_event_missing_official_required_fields, websocket_event_shape_parse_error,
    websocket_event_to_sse_frame,
    websocket_function_call_output_item_event_invalid_required_fields,
    websocket_function_call_output_payload_item_event_invalid_required_fields,
    websocket_image_generation_call_output_item_event_invalid_required_fields,
    websocket_incomplete_response_reason,
    websocket_local_shell_call_output_item_event_invalid_required_fields,
    websocket_message_output_item_event_invalid_required_fields, websocket_metadata_turn_state,
    websocket_output_item_event_invalid_item_type_tag,
    websocket_output_item_event_invalid_metadata, websocket_output_item_event_missing_item,
    websocket_output_item_event_non_object_item, websocket_parity_diff,
    websocket_payload_audit_snapshot,
    websocket_reasoning_output_item_event_invalid_required_fields,
    websocket_reasoning_summary_part_added_missing_summary_index,
    websocket_response_completed_missing_response, websocket_response_completed_parse_error,
    websocket_response_created_missing_response,
    websocket_response_output_text_delta_missing_delta,
    websocket_tool_search_call_output_item_event_invalid_required_fields,
    websocket_tool_search_output_item_event_invalid_required_fields,
    websocket_web_search_call_output_item_event_invalid_required_fields, OpeningAuditHeader,
    OpeningAuditSnapshot, PayloadAuditSnapshot, WebSocketAuditErrorSnapshot,
    WebSocketErrorClassificationProfile,
};
use codex_proxy_rs::gateway::dispatch::responses::{
    apply_response_model_options, http_sse_fallback_allowed, transport_for_request, CodexTransport,
};
use codex_proxy_rs::gateway::openai::responses::{
    response_failed_sse_event, response_from_codex_sse, translate_response_to_codex,
    translate_response_to_compact, CollectedResponse, OpenAiResponsesRequest,
};
use serde_json::json;

mod protocol_codex_websocket;
mod protocol_openai_chat;
mod protocol_openai_responses;
mod protocol_usage_rate_limits;

fn assert_substrings_appear_in_order(haystack: &str, needles: &[&str]) {
    let mut cursor = 0;
    for needle in needles {
        let Some(offset) = haystack[cursor..].find(needle) else {
            panic!("expected substring {needle:?} after byte {cursor} in:\n{haystack}");
        };
        cursor += offset + needle.len();
    }
}
