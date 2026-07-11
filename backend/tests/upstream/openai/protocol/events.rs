use super::*;

#[test]
fn extract_usage_should_read_codex_usage_shape() {
    let body = json!({
        "usage": {
            "input_tokens": 12,
            "output_tokens": 5,
            "input_tokens_details": {
                "cached_tokens": 3
            }
        }
    });

    let usage = extract_usage(&body).expect("usage should exist");

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 3,
            reasoning_tokens: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 17,
        }
    );
}

#[test]
fn extract_usage_should_read_reasoning_and_total_tokens() {
    let body = json!({
        "usage": {
            "input_tokens": 12,
            "output_tokens": 8,
            "total_tokens": 20,
            "output_tokens_details": {
                "reasoning_tokens": 6
            }
        }
    });

    let usage = extract_usage(&body).expect("usage should exist");

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 12,
            output_tokens: 8,
            cached_tokens: 0,
            reasoning_tokens: 6,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 20,
        }
    );
}

#[test]
fn extract_usage_should_read_image_generation_tokens_separately() {
    let body = json!({
        "usage": {
            "input_tokens": 12,
            "output_tokens": 5,
            "input_tokens_details": {
                "cached_tokens": 3
            }
        },
        "tool_usage": {
            "image_gen": {
                "input_tokens": 31,
                "output_tokens": 9
            }
        }
    });

    let usage = extract_usage(&body).expect("usage should exist");

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 3,
            reasoning_tokens: 0,
            image_input_tokens: 31,
            image_output_tokens: 9,
            total_tokens: 17,
        }
    );
}

#[test]
fn extract_usage_should_read_openai_usage_shape() {
    let usage = extract_usage(&json!({
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 4,
            "prompt_tokens_details": {
                "cached_tokens": 2
            }
        }
    }))
    .expect("usage should exist");

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 8,
            output_tokens: 4,
            cached_tokens: 2,
            reasoning_tokens: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 12,
        }
    );
}

#[test]
fn extract_sse_usage_should_prefer_completed_response_usage() {
    let body = include_str!("../../../fixtures/responses/http_sse/created_completed_usage.sse");

    let usage = extract_sse_usage(body)
        .expect("usage extraction should succeed")
        .expect("usage should exist");

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 3,
            output_tokens: 5,
            cached_tokens: 1,
            reasoning_tokens: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 8,
        }
    );
}

#[test]
fn extract_sse_usage_should_read_completed_image_generation_tokens() {
    let body = include_str!("../../../fixtures/responses/http_sse/completed_image_tool_usage.sse");

    let usage = extract_sse_usage(body)
        .expect("usage extraction should succeed")
        .expect("usage should exist");

    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 12,
            output_tokens: 5,
            cached_tokens: 3,
            reasoning_tokens: 0,
            image_input_tokens: 31,
            image_output_tokens: 9,
            total_tokens: 17,
        }
    );
}

#[test]
fn retry_after_seconds_from_body_should_read_structured_retry_delay() {
    let body = json!({
        "response": {
            "error": {
                "resets_in_seconds": 45
            }
        }
    })
    .to_string();

    assert_eq!(retry_after_seconds_from_body(&body), Some(45));
}

#[test]
fn retry_after_seconds_from_body_should_parse_rate_limit_message_seconds() {
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
fn retry_after_seconds_from_body_should_parse_rate_limit_message_milliseconds() {
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
fn retry_after_seconds_from_body_should_ignore_retry_message_for_other_codes() {
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
fn parse_rate_limit_headers_should_extract_primary_secondary_and_review_windows() {
    let headers = vec![
        (
            "x-codex-primary-used-percent".to_string(),
            "100".to_string(),
        ),
        (
            "x-codex-primary-window-minutes".to_string(),
            "5".to_string(),
        ),
        (
            "x-codex-primary-reset-at".to_string(),
            "1893456300".to_string(),
        ),
        (
            "x-codex-secondary-used-percent".to_string(),
            "42.5".to_string(),
        ),
        (
            "x-codex-secondary-window-minutes".to_string(),
            "10080".to_string(),
        ),
        (
            "x-codex-code-review-primary-used-percent".to_string(),
            "80".to_string(),
        ),
        (
            "x-codex-code-review-primary-reset-at".to_string(),
            "1893456600".to_string(),
        ),
    ];

    let parsed = parse_rate_limit_headers(&headers).expect("rate limits should parse");

    let core = parsed.limits.get("codex").expect("core rate limit");
    assert_eq!(
        core.primary,
        Some(RateLimitWindow {
            used_percent: 100.0,
            window_minutes: Some(5),
            reset_at: Some(1_893_456_300),
        })
    );
    assert_eq!(
        core.secondary.expect("secondary window").window_minutes,
        Some(10080)
    );
    assert_eq!(
        parsed
            .limits
            .get("codex_code_review")
            .expect("review window")
            .primary
            .expect("review primary")
            .reset_at,
        Some(1_893_456_600)
    );
}

#[test]
fn parse_rate_limits_event_should_extract_internal_websocket_rate_limits() {
    let event = json!({
        "type": "codex.rate_limits",
        "rate_limits": {
            "primary": {
                "used_percent": 99.5,
                "window_minutes": 300,
                "reset_at": 1893456300
            },
            "secondary": {
                "used_percent": 10,
                "window_minutes": 10080,
                "reset_at": 1894056000
            }
        }
    });

    let parsed = parse_rate_limits_event(&event).expect("event should parse");

    let core = parsed.limits.get("codex").expect("core rate limit");
    let used_percent = core.primary.expect("primary window").used_percent;
    assert!((used_percent - 99.5).abs() < f64::EPSILON);
    assert_eq!(
        core.secondary.expect("secondary window").reset_at,
        Some(1_894_056_000)
    );
}

#[test]
fn parse_rate_limit_headers_should_extract_arbitrary_limits_and_account_metadata() {
    let headers = vec![
        (
            "x-codex-other-primary-used-percent".to_string(),
            "63.5".to_string(),
        ),
        (
            "x-codex-other-primary-window-minutes".to_string(),
            "1440".to_string(),
        ),
        (
            "x-codex-other-limit-name".to_string(),
            "Codex Other".to_string(),
        ),
        (
            "x-codex-credits-has-credits".to_string(),
            "true".to_string(),
        ),
        ("x-codex-credits-unlimited".to_string(), "false".to_string()),
        ("x-codex-credits-balance".to_string(), "12.50".to_string()),
        ("x-codex-plan-type".to_string(), "pro".to_string()),
        ("x-codex-promo-message".to_string(), "bonus".to_string()),
        (
            "x-codex-rate-limit-reached-type".to_string(),
            "weekly".to_string(),
        ),
    ];

    let parsed = parse_rate_limit_headers(&headers).expect("rate limits should parse");
    let other = parsed.limits.get("codex_other").expect("dynamic limit");
    assert_eq!(other.limit_name.as_deref(), Some("Codex Other"));
    assert_eq!(
        other.primary.expect("primary window").window_minutes,
        Some(1440)
    );
    let credits = parsed.credits.expect("credits");
    assert!(credits.has_credits);
    assert!(!credits.unlimited);
    assert_eq!(credits.balance.as_deref(), Some("12.50"));
    assert_eq!(parsed.plan_type.as_deref(), Some("pro"));
    assert_eq!(parsed.promo_message.as_deref(), Some("bonus"));
    assert_eq!(parsed.rate_limit_reached_type.as_deref(), Some("weekly"));
}

#[test]
fn parse_rate_limits_event_should_keep_metered_limit_credits_and_plan() {
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

    let parsed = parse_rate_limits_event(&event).expect("event should parse");
    let other = parsed.limits.get("codex_other").expect("metered limit");
    assert_eq!(other.limit_name.as_deref(), Some("Other requests"));
    assert_eq!(parsed.plan_type.as_deref(), Some("team"));
    assert_eq!(
        parsed.credits.expect("credits").balance.as_deref(),
        Some("9.25")
    );
}

#[test]
fn rate_limit_quota_should_preserve_existing_monthly_and_credits_when_passive_data_lacks_them() {
    let headers = vec![
        ("x-codex-primary-used-percent".to_string(), "25".to_string()),
        (
            "x-codex-primary-window-minutes".to_string(),
            "5".to_string(),
        ),
        (
            "x-codex-primary-reset-at".to_string(),
            "1893456300".to_string(),
        ),
    ];
    let existing = json!({
        "monthly_limit": {
            "key": "spend-control-monthly",
            "source": "spend_control",
            "used_percent": 52,
            "remaining_percent": 48,
            "reset_at": 1896048000,
            "window_minutes": 43200,
            "limit_reached": false
        },
        "credits": {
            "has_credits": true,
            "unlimited": false,
            "balance": 12
        }
    });
    let parsed = parse_rate_limit_headers(&headers).expect("rate limits should parse");

    let quota = rate_limit_quota(&parsed, Some("plus"), Some(&existing));

    assert_eq!(quota["plan_type"], "plus");
    assert_eq!(quota["snapshots"][0]["primary"]["remaining_percent"], 75);
    assert_eq!(quota["monthly_limit"]["used_percent"], 52);
    assert_eq!(quota["credits"]["balance"], 12);
}

#[test]
fn rate_limit_quota_should_block_when_window_is_exhausted_even_if_flag_is_false() {
    let event = json!({
        "type": "codex.rate_limits",
        "rate_limits": {
            "limit_reached": false,
            "primary": {
                "used_percent": 100,
                "window_minutes": 300,
                "reset_at": 1893456300
            }
        }
    });
    let parsed = parse_rate_limits_event(&event).expect("event should parse");

    let quota = rate_limit_quota(&parsed, Some("plus"), None);

    assert_eq!(quota["snapshots"][0]["blocked"], true);
}

#[tokio::test]
async fn refresh_scheduler_should_refresh_before_expiry_and_preserve_refresh_token() {
    use async_trait::async_trait;
    use chrono::{Duration as ChronoDuration, Utc};
    use codex_proxy_rs::fleet::account::AccountStatus;
    use codex_proxy_rs::{
        fleet::refresh::{RefreshPolicy, RefreshScheduler},
        upstream::openai::token_client::{RefreshFailure, TokenPair, TokenRefresher},
    };

    #[derive(Clone)]
    struct StaticRefreshClient {
        result: Result<TokenPair, RefreshFailure>,
    }

    #[async_trait]
    impl TokenRefresher for StaticRefreshClient {
        async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
            self.result.clone()
        }
    }

    let now = Utc::now();
    let mut account = crate::support::accounts::test_account("acct_1", AccountStatus::Active);
    account.access_token_expires_at = Some(now + ChronoDuration::seconds(60));
    account.refresh_token = Some("rt_keep".to_string());
    let scheduler = RefreshScheduler::new(
        RefreshPolicy {
            refresh_margin_seconds: 300,
            refresh_concurrency: 2,
        },
        StaticRefreshClient {
            result: Ok(TokenPair {
                access_token: "new-access".to_string(),
                refresh_token: None,
            }),
        },
    );

    let refreshed = scheduler
        .refresh_account_at(&account, now)
        .await
        .expect("refresh should succeed");

    assert_eq!(refreshed.access_token, "new-access");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("rt_keep"));
    assert_eq!(refreshed.status, AccountStatus::Active);
}
