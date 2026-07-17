//! OpenAI 账号 usage/probe adapter。

use std::{collections::VecDeque, time::Duration};

use async_trait::async_trait;
use futures::StreamExt;
use secrecy::ExposeSecret;
use serde_json::{Value, json};

use crate::fleet::{
    account_gateway::{
        AccountFailureObservation, AccountGatewayError, AccountProbeEvent, AccountProbeRequest,
        AccountProbeSession, AccountUpstreamContext, AccountUpstreamGateway, AccountUsageResult,
    },
    quota::{
        QuotaCreditsObservation, QuotaLimitObservation, QuotaObservation, QuotaSpendControl,
        QuotaWindowObservation, quota_from_observation,
    },
};

use super::{
    failure::{UpstreamFailureFacts, upstream_failure_facts},
    protocol::{
        responses::{CodexResponsesRequest, ResponsesSseFailure},
        sse::{SseEvent, parse_sse_events},
    },
    transport::{CodexBackendClient, CodexBackendSseStream, CodexRequestContext},
};

#[async_trait]
impl AccountUpstreamGateway for CodexBackendClient {
    async fn fetch_usage(
        &self,
        context: AccountUpstreamContext,
    ) -> Result<AccountUsageResult, AccountGatewayError> {
        let raw = CodexBackendClient::fetch_usage(self, codex_context(&context))
            .await
            .map_err(account_gateway_error)?;
        Ok(account_usage_result(raw))
    }

    async fn probe_response(
        &self,
        context: AccountUpstreamContext,
        request: AccountProbeRequest,
    ) -> Result<AccountProbeSession, AccountGatewayError> {
        let mut codex_request = CodexResponsesRequest::new_http_sse(
            request.model,
            request.instructions,
            vec![json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": request.input_text }]
            })],
        );
        codex_request.set_stream(true);
        codex_request.set_store(false);
        codex_request.force_http_sse = true;
        let request_payload = serde_json::to_value(&codex_request)
            .map_err(|error| AccountGatewayError::new(error.to_string(), None))?;
        let response = self
            .create_response_stream(&codex_request, codex_context(&context))
            .await
            .map_err(account_gateway_error)?;
        let state = ProbeStreamState::new(response.body);
        let events = futures::stream::unfold(state, next_probe_event);
        Ok(AccountProbeSession {
            request_payload,
            events: Box::pin(events),
        })
    }

    async fn evict_account_connections(&self, account_id: &str) {
        self.evict_websocket_account(account_id).await;
    }
}

fn codex_context(context: &AccountUpstreamContext) -> CodexRequestContext<'_> {
    CodexRequestContext {
        access_token: context.access_token.expose_secret(),
        account_id: context.account_id.as_deref(),
        request_id: &context.request_id,
        turn_state: None,
        turn_metadata: None,
        beta_features: None,
        include_timing_metrics: None,
        version: None,
        codex_window_id: None,
        parent_thread_id: None,
        cookie_header: context.cookie_header.as_deref(),
        installation_id: context.installation_id.as_deref(),
        session_id: None,
        thread_id: None,
        client_request_id: None,
        turn_id: None,
    }
}

fn account_usage_result(raw: Value) -> AccountUsageResult {
    let observation = usage_quota_observation(&raw);
    AccountUsageResult {
        quota: quota_from_observation(&observation, None, None),
        account_id: string_field(&raw, "account_id"),
        user_id: string_field(&raw, "user_id"),
        email: string_field(&raw, "email"),
        plan_type: string_field(&raw, "plan_type"),
        raw,
    }
}

fn usage_quota_observation(usage: &Value) -> QuotaObservation {
    let mut limits = std::collections::BTreeMap::new();
    if let Some(limit) = usage
        .get("rate_limit")
        .and_then(|rate_limit| usage_limit_observation("codex", None, None, rate_limit))
    {
        limits.insert("codex".to_string(), limit);
    }
    if let Some(additional) = usage
        .get("additional_rate_limits")
        .and_then(Value::as_array)
    {
        for item in additional {
            let limit_name = item.get("limit_name").and_then(trimmed_str);
            let metered_feature = item.get("metered_feature").and_then(trimmed_str);
            let Some(limit_id) = metered_feature.or(limit_name).map(normalize_limit_id) else {
                continue;
            };
            let Some(limit) = item.get("rate_limit").and_then(|rate_limit| {
                usage_limit_observation(&limit_id, limit_name, metered_feature, rate_limit)
            }) else {
                continue;
            };
            limits.insert(limit_id, limit);
        }
    }
    QuotaObservation {
        limits,
        credits: usage
            .get("credits")
            .filter(|value| !value.is_null())
            .map(|credits| QuotaCreditsObservation {
                has_credits: credits
                    .get("has_credits")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                unlimited: credits
                    .get("unlimited")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                overage_limit_reached: credits
                    .get("overage_limit_reached")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                balance: credits
                    .get("balance")
                    .filter(|value| !value.is_null())
                    .cloned(),
            }),
        spend_control: usage
            .get("spend_control")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::from_value::<QuotaSpendControl>(value.clone()).ok()),
        plan_type: string_field(usage, "plan_type"),
        ..QuotaObservation::default()
    }
}

fn usage_limit_observation(
    limit_id: &str,
    limit_name: Option<&str>,
    metered_feature: Option<&str>,
    rate_limit: &Value,
) -> Option<QuotaLimitObservation> {
    if rate_limit.is_null() {
        return None;
    }
    let primary = rate_limit
        .get("primary_window")
        .and_then(usage_window_observation);
    let secondary = rate_limit
        .get("secondary_window")
        .and_then(usage_window_observation);
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    Some(QuotaLimitObservation {
        limit_id: limit_id.to_string(),
        limit_name: limit_name.map(ToString::to_string),
        metered_feature: metered_feature.map(ToString::to_string),
        allowed: rate_limit.get("allowed").and_then(Value::as_bool),
        limit_reached: rate_limit.get("limit_reached").and_then(Value::as_bool),
        primary,
        secondary,
    })
}

fn usage_window_observation(window: &Value) -> Option<QuotaWindowObservation> {
    if window.is_null() {
        return None;
    }
    let used_percent = window
        .get("used_percent")
        .and_then(number_value)
        .unwrap_or(0.0);
    let window_minutes = window
        .get("limit_window_seconds")
        .and_then(number_value)
        .filter(|seconds| *seconds > 0.0)
        .and_then(|seconds| Duration::try_from_secs_f64((seconds / 60.0).round()).ok())
        .map(|duration| duration.as_secs());
    Some(QuotaWindowObservation {
        used_percent,
        reset_at: window.get("reset_at").and_then(positive_i64),
        window_minutes,
        used: first_present(
            window,
            &[
                "used_tokens",
                "used",
                "used_credits",
                "usage",
                "consumed_tokens",
                "consumed",
            ],
        ),
        limit: first_present(
            window,
            &[
                "limit_tokens",
                "limit",
                "limit_credits",
                "quota",
                "total_tokens",
                "total",
            ],
        ),
    })
}

struct ProbeStreamState {
    body: CodexBackendSseStream,
    buffer: String,
    pending: VecDeque<AccountProbeEvent>,
    terminal: bool,
    eof: bool,
}

impl ProbeStreamState {
    fn new(body: CodexBackendSseStream) -> Self {
        Self {
            body,
            buffer: String::new(),
            pending: VecDeque::new(),
            terminal: false,
            eof: false,
        }
    }
}

async fn next_probe_event(
    mut state: ProbeStreamState,
) -> Option<(AccountProbeEvent, ProbeStreamState)> {
    loop {
        if let Some(event) = state.pending.pop_front() {
            return Some((event, state));
        }
        if state.terminal {
            return None;
        }
        if let Some(frame) = take_sse_frame(&mut state.buffer) {
            process_probe_frame(&mut state, &frame);
            continue;
        }
        if state.eof {
            if !state.buffer.trim().is_empty() {
                let frame = std::mem::take(&mut state.buffer);
                process_probe_frame(&mut state, &frame);
                continue;
            }
            state.terminal = true;
            return Some((
                AccountProbeEvent::Failed(AccountGatewayError::new(
                    "Stream ended before response.completed",
                    None,
                )),
                state,
            ));
        }
        match state.body.next().await {
            Some(Ok(chunk)) => state.buffer.push_str(&String::from_utf8_lossy(&chunk)),
            Some(Err(error)) => {
                state.terminal = true;
                return Some((
                    AccountProbeEvent::Failed(account_gateway_error(error)),
                    state,
                ));
            }
            None => state.eof = true,
        }
    }
}

fn process_probe_frame(state: &mut ProbeStreamState, frame: &str) {
    let events = match parse_sse_events(frame) {
        Ok(events) => events,
        Err(error) => {
            state.terminal = true;
            state
                .pending
                .push_back(AccountProbeEvent::Failed(AccountGatewayError::new(
                    error.to_string(),
                    None,
                )));
            return;
        }
    };
    for event in events {
        process_probe_sse_event(state, &event);
        if state.terminal {
            break;
        }
    }
}

fn process_probe_sse_event(state: &mut ProbeStreamState, event: &SseEvent) {
    let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
        return;
    };
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str)
                && !delta.is_empty()
            {
                state
                    .pending
                    .push_back(AccountProbeEvent::Content(delta.to_string()));
            }
        }
        Some("response.completed" | "response.done") => {
            state.terminal = true;
            state.pending.push_back(AccountProbeEvent::Complete);
        }
        Some(event_name @ ("response.failed" | "error")) => {
            let failure = ResponsesSseFailure::from_event(event_name, &value);
            state.terminal = true;
            state
                .pending
                .push_back(AccountProbeEvent::Failed(response_gateway_error(&failure)));
        }
        _ => {}
    }
}

fn account_gateway_error(error: super::transport::CodexClientError) -> AccountGatewayError {
    let message = error.to_string();
    let facts = upstream_failure_facts(&error);
    AccountGatewayError::new(message, Some(account_failure_observation(&facts)))
}

fn response_gateway_error(failure: &ResponsesSseFailure) -> AccountGatewayError {
    AccountGatewayError::new(
        failure.message.clone(),
        Some(AccountFailureObservation {
            status_code: failure.explicit_status_code,
            code: failure.upstream_code.clone(),
            error_type: failure.upstream_type.clone(),
            message: failure.message.clone(),
            body: failure.message.clone(),
            retry_after_seconds: failure.retry_after_seconds,
            ..AccountFailureObservation::default()
        }),
    )
}

fn account_failure_observation(facts: &UpstreamFailureFacts) -> AccountFailureObservation {
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

fn take_sse_frame(buffer: &mut String) -> Option<String> {
    let index = buffer.find("\n\n").or_else(|| buffer.find("\r\n\r\n"))?;
    let delimiter_len = if buffer[index..].starts_with("\r\n\r\n") {
        4
    } else {
        2
    };
    let frame = buffer[..index + delimiter_len].to_string();
    buffer.drain(..index + delimiter_len);
    Some(frame)
}

fn normalize_limit_id(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(trimmed_str)
        .map(ToString::to_string)
}

fn trimmed_str(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn first_present(value: &Value, keys: &[&str]) -> Option<Value> {
    keys.iter()
        .find_map(|key| value.get(*key).filter(|value| !value.is_null()).cloned())
}

fn number_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
        .filter(|value| value.is_finite())
}

fn positive_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .filter(|value| *value > 0)
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .filter(|value| *value > 0)
}
