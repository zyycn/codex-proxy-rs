use std::sync::Arc;

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
fn account_score_failure_filter_should_include_server_overload_as_a_regular_reason() {
    let error = sent_error(ProviderErrorKind::Unavailable, Some("server_is_overloaded"));

    assert!(openai_failure_affects_account_score(&error));
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
    let error = ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
        .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
            "upstream unavailable",
            None,
            Some("SERVICE_UNAVAILABLE_ERROR".to_owned()),
        ));

    assert!(openai_failure_affects_account_score(&error));
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
fn openai_feedback_should_score_server_overload_as_one_regular_failure() {
    let feedback = Arc::new(AccountFeedbackStats::default());
    let provider = ProviderKind::new("openai").expect("provider");
    let account = ProviderAccountId::new("acct_overloaded").expect("account");

    deliver_error_with_openai_feedback(
        &feedback,
        &provider,
        &account,
        sent_error(ProviderErrorKind::Unavailable, Some("server_is_overloaded")),
    );

    assert_eq!(
        feedback.scheduling_signals(&provider, &account).0,
        Some(2_000)
    );
}
