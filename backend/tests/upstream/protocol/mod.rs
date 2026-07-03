use codex_proxy_rs::proxy::openai::responses::{
    response_failed_sse_event, translate_response_to_codex, translate_response_to_compact,
    OpenAiResponsesRequest,
};
use codex_proxy_rs::upstream::models::ParsedModelName;
use codex_proxy_rs::upstream::protocol::events::{
    extract_sse_usage, extract_usage, parse_rate_limit_headers, parse_rate_limits_event,
    rate_limit_quota, retry_after_seconds_from_body, RateLimitWindow, TokenUsage,
};
use codex_proxy_rs::upstream::protocol::responses::{
    apply_response_model_options, http_sse_fallback_allowed, response_body_has_first_event,
    response_from_codex_sse, response_sse_event_is_terminal, transport_for_request,
    CodexResponsesRequest, CodexTransport, CollectedResponse,
};
use codex_proxy_rs::upstream::protocol::sse::{
    parse_sse_events, sse_body_has_done, DONE_SSE_FRAME,
};
use codex_proxy_rs::upstream::protocol::websocket::{
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
    websocket_output_item_event_non_object_item, websocket_payload_audit_snapshot,
    websocket_reasoning_output_item_event_invalid_required_fields,
    websocket_reasoning_summary_part_added_missing_summary_index,
    websocket_response_completed_missing_response, websocket_response_completed_parse_error,
    websocket_response_created_missing_response,
    websocket_response_output_text_delta_missing_delta,
    websocket_tool_search_call_output_item_event_invalid_required_fields,
    websocket_tool_search_output_item_event_invalid_required_fields,
    websocket_web_search_call_output_item_event_invalid_required_fields, OpeningAuditHeader,
    OpeningAuditSnapshot,
};
use serde_json::json;

use crate::support::assertions::assert_substrings_appear_in_order;

mod codex_websocket;
mod openai_chat;
mod openai_responses;
mod response_first_event;
mod usage_rate_limits;
