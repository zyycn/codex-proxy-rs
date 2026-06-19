use codex_proxy_core::{
    gateway::{
        fingerprint::Fingerprint,
        installation::{generate_installation_id, parse_installation_id},
    },
    protocol::{
        codex::{
            events::{
                cooldown_with_jitter, extract_usage, parse_rate_limit_headers,
                retry_after_seconds_from_body, ParsedRateLimits, TokenUsage,
            },
            responses::CodexResponsesRequest,
        },
        openai::{
            chat::{translate_chat_to_codex, ChatCompletionRequest, ChatMessage},
            responses::ResponseFormat,
        },
    },
};

#[test]
fn gateway_exports_core_protocol_and_identity_modules() {
    let _fingerprint_type = std::any::type_name::<Fingerprint>();
    let _response_format_type = std::any::type_name::<ResponseFormat>();
    let _request_type = std::any::type_name::<CodexResponsesRequest>();
    let _usage_type = std::any::type_name::<TokenUsage>();
    let _rate_limits_type = std::any::type_name::<ParsedRateLimits>();
    let _translate_fn = translate_chat_to_codex;
    let _extract_usage_fn = extract_usage;
    let _parse_rate_limit_headers_fn = parse_rate_limit_headers;
    let _retry_after_fn = retry_after_seconds_from_body;
    let _cooldown_with_jitter_fn = cooldown_with_jitter;
    let _chat_request_type = std::any::type_name::<ChatCompletionRequest>();
    let _chat_message_type = std::any::type_name::<ChatMessage>();
    let _generate_installation_id_fn = generate_installation_id;
    let _parse_installation_id_fn = parse_installation_id;
}
