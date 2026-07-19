use gateway_protocol::openai::events::{
    RateLimitWindow, TokenUsage, extract_sse_usage, extract_usage, is_rate_limit_header_name,
    parse_rate_limit_headers, parse_rate_limits_event, rate_limits_to_header_pairs,
    retry_after_seconds_from_body,
};
use serde_json::json;

#[test]
fn extract_usage_should_read_codex_usage_shape() {
    let body = json!({
        "usage": {
            "input_tokens": 12,
            "output_tokens": 5,
            "input_tokens_details": {
                "cached_tokens": 3,
                "cache_write_tokens": 4
            }
        }
    });

    assert_eq!(
        extract_usage(&body),
        Some(TokenUsage {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 3,
            cache_write_tokens: 4,
            reasoning_tokens: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 17,
        })
    );
}

#[test]
fn extract_usage_should_read_openai_usage_shape() {
    let body = json!({
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 4,
            "prompt_tokens_details": {"cached_tokens": 2},
            "completion_tokens_details": {"reasoning_tokens": 3}
        }
    });

    assert_eq!(
        extract_usage(&body),
        Some(TokenUsage {
            input_tokens: 8,
            output_tokens: 4,
            cached_tokens: 2,
            cache_write_tokens: 0,
            reasoning_tokens: 3,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 12,
        })
    );
}

#[test]
fn extract_usage_should_preserve_explicit_total_and_reasoning_tokens() {
    let body = json!({
        "usage": {
            "input_tokens": 12,
            "output_tokens": 8,
            "total_tokens": 20,
            "output_tokens_details": {"reasoning_tokens": 6}
        }
    });

    assert_eq!(
        extract_usage(&body),
        Some(TokenUsage {
            input_tokens: 12,
            output_tokens: 8,
            cached_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 6,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 20,
        })
    );
}

#[test]
fn extract_usage_should_keep_image_tool_usage_separate_from_model_total() {
    let body = json!({
        "usage": {
            "input_tokens": 12,
            "output_tokens": 5,
            "total_tokens": 17
        },
        "tool_usage": {
            "image_gen": {
                "input_tokens": 31,
                "output_tokens": 9
            }
        }
    });

    assert_eq!(
        extract_usage(&body),
        Some(TokenUsage {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            image_input_tokens: 31,
            image_output_tokens: 9,
            total_tokens: 17,
        })
    );
}

#[test]
fn extract_usage_should_reject_objects_without_usage_fields() {
    assert_eq!(extract_usage(&json!({"status": "completed"})), None);
}

#[test]
fn extract_sse_usage_should_prefer_completed_response_usage() {
    let body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n\n",
        "data: [DONE]\n\n",
    );

    assert_eq!(
        extract_sse_usage(body).expect("valid SSE"),
        Some(TokenUsage {
            input_tokens: 3,
            output_tokens: 5,
            cached_tokens: 1,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 8,
        })
    );
}

#[test]
fn extract_sse_usage_should_fall_back_to_last_visible_usage() {
    let body = concat!(
        "event: response.created\n",
        "data: {\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\n",
        "event: response.output_text.done\n",
        "data: {\"usage\":{\"input_tokens\":4,\"output_tokens\":6}}\n\n",
    );

    assert_eq!(
        extract_sse_usage(body).expect("valid SSE"),
        Some(TokenUsage {
            input_tokens: 4,
            output_tokens: 6,
            cached_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 10,
        })
    );
}

#[test]
fn extract_sse_usage_should_preserve_completed_image_tool_usage() {
    let body = concat!(
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":3}},\"tool_usage\":{\"image_gen\":{\"input_tokens\":31,\"output_tokens\":9}}}}\n\n",
    );

    assert_eq!(
        extract_sse_usage(body).expect("valid SSE"),
        Some(TokenUsage {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 3,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            image_input_tokens: 31,
            image_output_tokens: 9,
            total_tokens: 17,
        })
    );
}

#[test]
fn extract_sse_usage_should_ignore_non_json_control_events() {
    let body = concat!(
        "event: ping\n",
        "data: still-alive\n\n",
        "event: response.completed\n",
        "data: {\"response\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n",
    );

    assert_eq!(
        extract_sse_usage(body).expect("valid SSE"),
        Some(TokenUsage {
            input_tokens: 2,
            output_tokens: 3,
            cached_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 5,
        })
    );
}

#[test]
fn retry_after_should_read_structured_response_delay() {
    let body = json!({
        "response": {"error": {"resets_in_seconds": 45}}
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), Some(45));
}

#[test]
fn retry_after_should_read_nested_error_delay() {
    let body = json!({
        "type": "error",
        "error": {"retry_after_seconds": "19"}
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), Some(19));
}

#[test]
fn retry_after_should_read_case_insensitive_header_array() {
    let body = json!({
        "type": "error",
        "headers": {"Retry-After": ["37"]}
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), Some(37));
}

#[test]
fn retry_after_should_round_fractional_seconds_up() {
    let body = json!({
        "response": {
            "error": {
                "code": "rate_limit_exceeded",
                "message": "Rate limit reached. Please try again in 11.054s."
            }
        }
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), Some(12));
}

#[test]
fn retry_after_should_round_milliseconds_up_to_one_second() {
    let body = json!({
        "error": {
            "code": "rate_limit_exceeded",
            "message": "Rate limit reached. Please try again in 28ms."
        }
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), Some(1));
}

#[test]
fn retry_after_should_not_infer_retry_from_unrelated_error_message() {
    let body = json!({
        "response": {
            "error": {
                "code": "upstream_transient_error",
                "message": "Try again in 35 seconds."
            }
        }
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), None);
}

#[test]
fn retry_after_should_reject_malformed_json() {
    assert_eq!(retry_after_seconds_from_body("not-json"), None);
}

#[test]
fn parse_rate_limit_headers_should_extract_primary_and_secondary_windows() {
    let headers = vec![
        ("x-codex-primary-used-percent".to_owned(), "100".to_owned()),
        ("x-codex-primary-window-minutes".to_owned(), "5".to_owned()),
        (
            "x-codex-primary-reset-at".to_owned(),
            "1893456300".to_owned(),
        ),
        (
            "x-codex-secondary-used-percent".to_owned(),
            "42.5".to_owned(),
        ),
        (
            "x-codex-secondary-window-minutes".to_owned(),
            "10080".to_owned(),
        ),
    ];

    let parsed = parse_rate_limit_headers(&headers).expect("rate-limit headers");
    let details = parsed.limits.get("codex").expect("Codex limit");

    assert_eq!(
        (details.primary, details.secondary),
        (
            Some(RateLimitWindow {
                used_percent: 100.0,
                window_minutes: Some(5),
                reset_at: Some(1_893_456_300),
            }),
            Some(RateLimitWindow {
                used_percent: 42.5,
                window_minutes: Some(10_080),
                reset_at: None,
            }),
        )
    );
}

#[test]
fn parse_rate_limit_headers_should_preserve_dynamic_limit_and_account_metadata() {
    let headers = vec![
        (
            "x-codex-other-primary-used-percent".to_owned(),
            "63.5".to_owned(),
        ),
        ("x-codex-active-limit".to_owned(), "codex-other".to_owned()),
        (
            "x-codex-other-limit-name".to_owned(),
            "Codex Other".to_owned(),
        ),
        ("x-codex-credits-has-credits".to_owned(), "true".to_owned()),
        ("x-codex-credits-unlimited".to_owned(), "false".to_owned()),
        ("x-codex-credits-balance".to_owned(), "12.50".to_owned()),
        ("x-codex-plan-type".to_owned(), "pro".to_owned()),
    ];

    let parsed = parse_rate_limit_headers(&headers).expect("rate-limit headers");
    let details = parsed.limits.get("codex_other").expect("dynamic limit");
    let credits = parsed.credits.expect("credits");

    assert_eq!(
        (
            parsed.active_limit.as_deref(),
            details.limit_name.as_deref(),
            credits.has_credits,
            credits.unlimited,
            credits.balance.as_deref(),
            parsed.plan_type.as_deref(),
        ),
        (
            Some("codex_other"),
            Some("Codex Other"),
            true,
            false,
            Some("12.50"),
            Some("pro"),
        )
    );
}

#[test]
fn parse_rate_limit_headers_should_preserve_a_code_review_window() {
    let headers = vec![
        (
            "x-codex-code-review-primary-used-percent".to_owned(),
            "80".to_owned(),
        ),
        (
            "x-codex-code-review-primary-reset-at".to_owned(),
            "1893456600".to_owned(),
        ),
    ];

    let parsed = parse_rate_limit_headers(&headers).expect("review rate limit");
    let review = parsed
        .limits
        .get("codex_code_review")
        .expect("review window");

    assert_eq!(
        review.primary,
        Some(RateLimitWindow {
            used_percent: 80.0,
            window_minutes: None,
            reset_at: Some(1_893_456_600),
        })
    );
}

#[test]
fn parse_rate_limits_event_should_extract_websocket_metered_limit() {
    let event = json!({
        "type": "codex.rate_limits",
        "plan_type": "team",
        "metered_limit_name": "codex-other",
        "limit_name": "Other requests",
        "rate_limits": {
            "primary": {
                "used_percent": 33,
                "window_minutes": 60,
                "reset_at": 1893456300
            }
        },
        "credits": {
            "has_credits": true,
            "unlimited": false,
            "balance": "9.25"
        }
    });

    let parsed = parse_rate_limits_event(&event).expect("rate-limit event");
    let details = parsed.limits.get("codex_other").expect("metered limit");

    assert_eq!(
        (
            parsed.active_limit.as_deref(),
            details.limit_name.as_deref(),
            details.primary.expect("primary window").window_minutes,
            parsed.plan_type.as_deref(),
            parsed.credits.expect("credits").balance,
        ),
        (
            Some("codex_other"),
            Some("Other requests"),
            Some(60),
            Some("team"),
            Some("9.25".to_owned()),
        )
    );
}

#[test]
fn parse_rate_limits_event_should_reject_unrelated_event_type() {
    assert_eq!(
        parse_rate_limits_event(&json!({
            "type": "response.completed",
            "rate_limits": {"primary": {"used_percent": 100}}
        })),
        None
    );
}

#[test]
fn rate_limit_header_round_trip_should_preserve_wire_facts() {
    let source = vec![
        ("x-codex-primary-used-percent".to_owned(), "75".to_owned()),
        (
            "x-codex-primary-window-minutes".to_owned(),
            "300".to_owned(),
        ),
        ("x-codex-active-limit".to_owned(), "codex".to_owned()),
        ("x-codex-credits-has-credits".to_owned(), "true".to_owned()),
        ("x-codex-credits-unlimited".to_owned(), "false".to_owned()),
        ("x-codex-plan-type".to_owned(), "plus".to_owned()),
    ];
    let parsed = parse_rate_limit_headers(&source).expect("initial parse");
    let encoded = rate_limits_to_header_pairs(&parsed);

    assert_eq!(parse_rate_limit_headers(&encoded), Some(parsed));
}

#[test]
fn rate_limit_header_filter_should_accept_only_supported_domain_headers() {
    assert_eq!(
        [
            "retry-after",
            "X-CoDeX-Primary-Used-Percent",
            "x-codex-plan-type",
            "content-type",
            "x-request-id",
        ]
        .map(is_rate_limit_header_name),
        [true, true, true, false, false]
    );
}
