use std::{sync::Arc, time::Duration};

use futures::future::BoxFuture;
use gateway_core::{
    engine::policy::{RequestPolicyFault, RetryDecision, RetryInput},
    upstream::UpstreamSendState,
};
use gateway_plugin_sdk::{
    SendState, Stage,
    call::policy::{RetryAction, RetryDecision as WireDecision, RetryDecisionRequest},
};

use super::RetryEntry;

pub(super) fn decide(
    entries: Arc<[RetryEntry]>,
    input: RetryInput,
    timeout: Duration,
) -> BoxFuture<'static, Result<RetryDecision, RequestPolicyFault>> {
    Box::pin(async move {
        let started = std::time::Instant::now();
        for entry in entries.iter().filter(|entry| {
            !input.extension_scope.contains(&entry.instance_id)
                && entry.scope.matches_processing(
                    &input.client_key_id,
                    &input.account_group_ids,
                    Some(&input.facts.provider),
                    input.facts.model.as_deref(),
                )
        }) {
            // 整条委托链共享预算，不能每个插件重新取得完整等待时间。
            let remaining = timeout
                .min(input.facts.remaining_deadline)
                .saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            match call(entry, &input, remaining).await {
                Ok(RetryDecision::Delegate) => {}
                Ok(decision) => return Ok(decision),
                Err(()) => tracing::warn!(
                    plugin_id = entry.plugin_id,
                    instance_id = entry.instance_id,
                    request_id = input.request_id.as_str(),
                    "插件重试策略无效或调用失败，委托后续策略"
                ),
            }
        }
        Ok(RetryDecision::Delegate)
    })
}

async fn call(
    entry: &RetryEntry,
    input: &RetryInput,
    timeout: Duration,
) -> Result<RetryDecision, ()> {
    let invocation = entry.invocation.as_ref().ok_or(())?;
    let mut allowed_actions = vec![RetryAction::Stop];
    if input.facts.retry_allowed {
        allowed_actions.push(RetryAction::Retry);
    }
    let request = RetryDecisionRequest {
        request_id: input.request_id.as_str().to_owned(),
        attempt_index: input.facts.attempt_index.get(),
        provider: input.facts.provider.as_str().to_owned(),
        model: input.facts.model.clone(),
        error_kind: input.facts.error_kind.as_str().to_owned(),
        upstream_status: input.facts.upstream_status,
        send_state: match input.facts.send_state {
            UpstreamSendState::NotSent => SendState::NotSent,
            UpstreamSendState::Sent => SendState::Sent,
            UpstreamSendState::Ambiguous => SendState::Ambiguous,
        },
        remaining_routing_attempts: input.facts.remaining_routing_attempts,
        remaining_deadline_ms: u64::try_from(input.facts.remaining_deadline.as_millis())
            .unwrap_or(u64::MAX),
        allowed_actions,
    };
    let mut context = invocation.session.context(Stage::Retry, timeout);
    context.request_id = Some(request.request_id.clone());
    context.attempt_id = Some(format!("{}:{}", request.request_id, request.attempt_index));
    let reply = invocation
        .session
        .call(
            "policy.retry_decision",
            context,
            serde_json::to_value(request).map_err(|_| ())?,
            vec![],
        )
        .await
        .map_err(|_| ())?;
    if !reply.payload.is_empty()
        || reply
            .result
            .as_object()
            .is_none_or(|fields| fields.len() != 1)
    {
        return Err(());
    }
    match serde_json::from_value::<WireDecision>(reply.result).map_err(|_| ())? {
        WireDecision::Delegate => Ok(RetryDecision::Delegate),
        WireDecision::Stop => Ok(RetryDecision::Stop),
        WireDecision::Retry if input.facts.retry_allowed => Ok(RetryDecision::Retry),
        WireDecision::Retry => Err(()),
    }
}
