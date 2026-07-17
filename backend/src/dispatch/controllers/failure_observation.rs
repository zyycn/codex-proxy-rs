//! 上游 typed failure 到 fleet 账号事实的边界映射。

use crate::{
    fleet::account_gateway::AccountFailureObservation,
    upstream::openai::{failure::UpstreamFailureFacts, protocol::responses::ResponsesSseFailure},
};

pub(super) fn from_upstream(facts: &UpstreamFailureFacts) -> AccountFailureObservation {
    AccountFailureObservation {
        status_code: facts.status_code,
        code: facts.code.clone(),
        error_type: facts.error_type.clone(),
        identity_authorization_error: facts.identity_authorization_error.clone(),
        identity_error_code: facts.identity_error_code.clone(),
        message: facts.message.clone(),
        body: facts.body.clone(),
        retry_after_seconds: facts.retry_after_seconds,
    }
}

pub(super) fn from_response(failure: &ResponsesSseFailure) -> AccountFailureObservation {
    AccountFailureObservation {
        status_code: failure.explicit_status_code,
        code: failure.upstream_code.clone(),
        error_type: failure.upstream_type.clone(),
        message: failure.message.clone(),
        body: failure.message.clone(),
        retry_after_seconds: failure.retry_after_seconds,
        ..AccountFailureObservation::default()
    }
}
