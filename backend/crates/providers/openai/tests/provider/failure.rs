//! 验证 OpenAI 上游失败分类与账号评分反馈规则

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt, stream};
use gateway_core::account::{AccountFeedbackStats, ProviderAccountId};
use gateway_core::engine::provider::{EventStream, ProviderCallMetadata, ProviderStream};
use gateway_core::error::{
    ClientVisibleUpstreamError, OpaqueUpstreamValue, ProviderError, ProviderErrorKind,
};
use gateway_core::routing::{ProviderKind, UpstreamModelId};
use gateway_core::upstream::{UpstreamSendState, UpstreamTransport};
use provider_openai::openai_failure_affects_account_score;

fn sent_error(kind: ProviderErrorKind, code: Option<&str>) -> ProviderError {
    let error = ProviderError::new(kind, UpstreamSendState::Sent);
    match code {
        Some(code) => error.with_upstream_code(OpaqueUpstreamValue::new(code)),
        None => error,
    }
}

fn deliver_error_with_openai_feedback(
    feedback: &Arc<AccountFeedbackStats>,
    provider: &ProviderKind,
    account: &ProviderAccountId,
    error: ProviderError,
) {
    let metadata = ProviderCallMetadata::new(
        provider.clone(),
        UpstreamModelId::new("gpt-5").expect("model"),
        account.clone(),
        UpstreamTransport::new("http_sse").expect("transport"),
    );
    let events: EventStream = Box::pin(stream::iter([Err(error)]));
    let mut provider_stream = ProviderStream::new(metadata, events, ())
        .with_filtered_account_feedback(Arc::clone(feedback), openai_failure_affects_account_score);

    futures::executor::block_on(async {
        assert!(
            provider_stream
                .next()
                .await
                .is_some_and(|event| event.is_err())
        );
    });
}

#[test]
fn account_score_failure_filter_should_accept_capacity_errors_with_scored_reasons() {
    for code in ["server_is_overloaded", "slow_down", "server_error"] {
        let error = sent_error(ProviderErrorKind::Unavailable, Some(code))
            .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
                "Selected model is at capacity. Please try a different model.",
                Some("server_error".to_owned()),
                Some("server_error".to_owned()),
            ));
        assert!(openai_failure_affects_account_score(&error));
    }
}

#[test]
fn account_score_failure_filter_should_accept_the_closed_reason_list() {
    for code in [
        "server_is_overloaded",
        "slow_down",
        "rate_limit_exceeded",
        "rate_limit_error",
        "server_error",
        "service_unavailable_error",
    ] {
        let error = sent_error(ProviderErrorKind::Unavailable, Some(code));
        assert!(
            openai_failure_affects_account_score(&error),
            "allowlisted code was rejected: {code}"
        );
    }
}

#[test]
fn account_score_failure_filter_should_normalize_case_and_whitespace() {
    let error = sent_error(ProviderErrorKind::Unavailable, Some("  SERVER_ERROR  "));

    assert!(openai_failure_affects_account_score(&error));
}

#[test]
fn account_score_failure_filter_should_use_a_structured_type_when_code_is_absent() {
    for error_type in [
        "SERVICE_UNAVAILABLE_ERROR",
        "SERVER_IS_OVERLOADED",
        "SLOW_DOWN",
    ] {
        let error = ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
            .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
                "upstream unavailable",
                None,
                Some(error_type.to_owned()),
            ));

        assert!(openai_failure_affects_account_score(&error));
    }
}

#[test]
fn account_score_failure_filter_should_prefer_a_structured_code_over_type() {
    let error = ProviderError::new(ProviderErrorKind::InvalidRequest, UpstreamSendState::Sent)
        .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
            "bad request",
            Some("invalid_request".to_owned()),
            Some("server_error".to_owned()),
        ));

    assert!(!openai_failure_affects_account_score(&error));
}

#[test]
fn account_score_failure_filter_should_reject_client_and_unknown_failures() {
    for error in [
        sent_error(ProviderErrorKind::InvalidRequest, Some("invalid_request")),
        sent_error(
            ProviderErrorKind::ContinuationRecoveryRequired,
            Some("previous_response_not_found"),
        ),
        sent_error(ProviderErrorKind::Unsupported, Some("unsupported")),
        sent_error(ProviderErrorKind::Unauthorized, Some("token_expired")),
        sent_error(ProviderErrorKind::QuotaExhausted, Some("quota_exhausted")),
        sent_error(ProviderErrorKind::RateLimited, Some("rate_limit_reached")),
        sent_error(
            ProviderErrorKind::Unavailable,
            Some("internal_server_error"),
        ),
        sent_error(ProviderErrorKind::Unavailable, Some("service_unavailable")),
        sent_error(
            ProviderErrorKind::Transport,
            Some("upstream_transport_error"),
        ),
        sent_error(ProviderErrorKind::Timeout, Some("first_output_timeout")),
        sent_error(
            ProviderErrorKind::Protocol,
            Some("upstream_stream_truncated"),
        ),
        sent_error(
            ProviderErrorKind::Unavailable,
            Some("upstream_empty_response"),
        ),
        sent_error(ProviderErrorKind::Unavailable, Some("new_unknown_reason")),
        sent_error(ProviderErrorKind::Transport, Some("new_unknown_reason")),
        sent_error(ProviderErrorKind::Transport, Some("")),
        ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
            .with_status(503),
    ] {
        assert!(
            !openai_failure_affects_account_score(&error),
            "non-allowlisted failure affected the account score"
        );
    }
}

#[test]
fn account_score_failure_filter_should_reject_internal_kinds_without_a_reason() {
    for kind in [
        ProviderErrorKind::Transport,
        ProviderErrorKind::Timeout,
        ProviderErrorKind::Protocol,
    ] {
        let error = sent_error(kind, None);
        assert!(
            !openai_failure_affects_account_score(&error),
            "unlisted internal failure affected the score: {}",
            kind.as_str()
        );
    }
}

#[test]
fn openai_feedback_should_not_amplify_repeated_client_configuration_errors() {
    let feedback = Arc::new(AccountFeedbackStats::default());
    let provider = ProviderKind::new("openai").expect("provider");
    let account = ProviderAccountId::new("acct_usable_account").expect("account");

    for _ in 0..100 {
        deliver_error_with_openai_feedback(
            &feedback,
            &provider,
            &account,
            sent_error(ProviderErrorKind::InvalidRequest, Some("invalid_request")),
        );
    }

    assert_eq!(
        feedback.scheduling_signals(&provider, &account),
        (None, None)
    );
}

#[test]
fn openai_feedback_should_penalize_capacity_rejections_even_with_transient_retry() {
    for code in ["server_is_overloaded", "slow_down"] {
        let feedback = Arc::new(AccountFeedbackStats::default());
        let provider = ProviderKind::new("openai").expect("provider");
        let account = ProviderAccountId::new("acct_overloaded").expect("account");
        let error = sent_error(ProviderErrorKind::UpstreamCapacityUnavailable, Some(code))
            .with_replay_safe()
            .with_transient_retry(
                NonZeroU32::new(3).expect("retry count"),
                Duration::from_millis(500),
                Duration::from_secs(8),
            );

        deliver_error_with_openai_feedback(&feedback, &provider, &account, error);

        assert_eq!(
            feedback.scheduling_signals(&provider, &account).0,
            Some(4_000),
            "capacity rejection should receive the stronger penalty: {code}"
        );
    }
}

#[test]
fn openai_feedback_should_keep_regular_server_error_penalty() {
    let feedback = Arc::new(AccountFeedbackStats::default());
    let provider = ProviderKind::new("openai").expect("provider");
    let account = ProviderAccountId::new("acct_server_error").expect("account");

    deliver_error_with_openai_feedback(
        &feedback,
        &provider,
        &account,
        sent_error(ProviderErrorKind::Unavailable, Some("server_error")),
    );

    assert_eq!(
        feedback.scheduling_signals(&provider, &account).0,
        Some(2_000)
    );
}

#[test]
fn openai_feedback_should_ignore_unconfirmed_capacity_rejections() {
    for send_state in [UpstreamSendState::NotSent, UpstreamSendState::Ambiguous] {
        let feedback = Arc::new(AccountFeedbackStats::default());
        let provider = ProviderKind::new("openai").expect("provider");
        let account = ProviderAccountId::new("acct_unconfirmed").expect("account");
        let error = ProviderError::new(ProviderErrorKind::UpstreamCapacityUnavailable, send_state)
            .with_upstream_code(OpaqueUpstreamValue::new("server_is_overloaded"));

        deliver_error_with_openai_feedback(&feedback, &provider, &account, error);

        assert_eq!(
            feedback.scheduling_signals(&provider, &account),
            (None, None)
        );
    }
}
