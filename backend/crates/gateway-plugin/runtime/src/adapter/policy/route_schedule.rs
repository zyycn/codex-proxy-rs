use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures::future::BoxFuture;
use gateway_core::{
    account::ProviderAccountId,
    engine::policy::{
        AccountScheduleDecision, AccountScheduleInput, ModelRouteDecision, ModelRouteInput,
        RequestPolicyFault, RequestPolicyPlan,
    },
    identity::ProviderKind,
    operation::{Operation, ProviderHttpHeader},
    routing::PublicModelId,
};
use gateway_plugin_sdk::{
    Stage,
    call::policy::{
        AccountScheduleCandidate as WireAccountScheduleCandidate,
        AccountScheduleDecision as WireAccountScheduleDecision,
        AccountScheduleRequest as WireAccountScheduleRequest,
        ModelRouteDecision as WireModelRouteDecision, ModelRouteRequest, PolicyHeader,
    },
};

use super::{AccountSchedulerEntry, ModelRouterEntry, PluginRequestPolicyPlan};

const MAX_HEADERS: usize = 128;
const MAX_HEADER_NAME_BYTES: usize = 128;
const MAX_HEADER_VALUE_BYTES: usize = 16 * 1024;
const MAX_HEADER_TOTAL_BYTES: usize = 64 * 1024;
const OPAQUE_HEADERS_KEY: &str = "opaque_request_headers";

impl RequestPolicyPlan for PluginRequestPolicyPlan {
    fn retry_decision(
        &self,
        input: gateway_core::engine::policy::RetryInput,
    ) -> BoxFuture<'static, Result<gateway_core::engine::policy::RetryDecision, RequestPolicyFault>>
    {
        super::retry::decide(self.retries.clone(), input, self.policy_timeout)
    }

    fn route_model(
        &self,
        input: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>> {
        let routers = std::sync::Arc::clone(&self.routers);
        let timeout = self.policy_timeout;
        Box::pin(async move {
            for entry in routers.iter().filter(|entry| {
                !input.suppresses_plugin(&entry.instance_id) && entry.scope.matches_route(&input)
            }) {
                let result = route_with_entry(entry, &input, timeout).await;
                match result {
                    Ok(ModelRouteDecision::Unhandled) => continue,
                    Ok(decision) => return Ok(decision),
                    Err(())
                        if entry.failure_policy
                            == gateway_admin::model::plugins::instances::PluginFailurePolicy::Delegate =>
                    {
                        tracing::warn!(
                            plugin_id = entry.plugin_id,
                            instance_id = entry.instance_id,
                            request_id = input.request_id().as_str(),
                            "插件模型路由失败，按绑定委托后续策略"
                        );
                    }
                    Err(()) => return Err(RequestPolicyFault),
                }
            }
            Ok(ModelRouteDecision::Unhandled)
        })
    }

    fn schedule_account(
        &self,
        input: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>> {
        let schedulers = std::sync::Arc::clone(&self.schedulers);
        let timeout = self.policy_timeout;
        Box::pin(async move {
            let Some(entry) = schedulers.iter().find(|entry| {
                !input.suppresses_plugin(&entry.instance_id) && entry.scope.matches_schedule(&input)
            }) else {
                return Ok(AccountScheduleDecision::Delegate);
            };
            match schedule_with_entry(entry, &input, timeout).await {
                Ok(decision) => Ok(decision),
                Err(())
                    if entry.failure_policy
                        == gateway_admin::model::plugins::instances::PluginFailurePolicy::Delegate =>
                {
                    tracing::warn!(
                        plugin_id = entry.plugin_id,
                        instance_id = entry.instance_id,
                        request_id = input.request_id().as_str(),
                        "插件账号调度失败，按绑定委托内置策略"
                    );
                    Ok(AccountScheduleDecision::Delegate)
                }
                Err(()) => Err(RequestPolicyFault),
            }
        })
    }
}

async fn route_with_entry(
    entry: &ModelRouterEntry,
    input: &ModelRouteInput,
    timeout: Duration,
) -> Result<ModelRouteDecision, ()> {
    let invocation = entry.invocation.as_ref().ok_or(())?;
    let projection = operation_projection(input.operation())?;
    let payload = if entry.requests_authorized {
        projection.body.to_bytes()?
    } else {
        Vec::new()
    };
    let headers = if entry.requests_authorized {
        project_request_headers(input.operation(), projection.context)?
    } else {
        Vec::new()
    };
    let request = ModelRouteRequest {
        request_id: input.request_id().as_str().to_owned(),
        operation: input.operation().kind().as_str().to_owned(),
        protocol: projection.protocol.to_owned(),
        model: input.requested_model().as_str().to_owned(),
        available_providers: input
            .available_providers()
            .iter()
            .map(|provider| provider.as_str().to_owned())
            .collect(),
        headers,
    };
    let mut context = invocation.session.context(Stage::Routing, timeout);
    context.request_id = Some(input.request_id().as_str().to_owned());
    let _network_scope = invocation
        .callbacks
        .prepare_data_plane(&context, input.execution_effects())
        .map_err(|_| ())?;
    let reply = invocation
        .session
        .call(
            "policy.route_model",
            context,
            serde_json::to_value(request).map_err(|_| ())?,
            payload,
        )
        .await
        .map_err(|_| ())?;
    if !reply.payload.is_empty() {
        return Err(());
    }
    let decision: WireModelRouteDecision = serde_json::from_value(reply.result).map_err(|_| ())?;
    match decision {
        WireModelRouteDecision::Unhandled => Ok(ModelRouteDecision::Unhandled),
        WireModelRouteDecision::Reject => Ok(ModelRouteDecision::Reject),
        WireModelRouteDecision::Route { provider, model } => {
            if provider.is_none() && model.is_none() {
                return Err(());
            }
            let provider = provider
                .map(|provider| ProviderKind::new(provider).map_err(|_| ()))
                .transpose()?;
            if provider
                .as_ref()
                .is_some_and(|provider| !input.available_providers().contains(provider))
            {
                return Err(());
            }
            let model = model
                .map(|model| PublicModelId::new(model).map_err(|_| ()))
                .transpose()?;
            Ok(ModelRouteDecision::Route { provider, model })
        }
    }
}

async fn schedule_with_entry(
    entry: &AccountSchedulerEntry,
    input: &AccountScheduleInput,
    timeout: Duration,
) -> Result<AccountScheduleDecision, ()> {
    let invocation = entry.invocation.as_ref().ok_or(())?;
    let highest_weight = input
        .candidates()
        .iter()
        .map(gateway_core::engine::policy::AccountScheduleCandidate::weight)
        .max()
        .ok_or(())?;
    let visible = input
        .candidates()
        .iter()
        .filter(|candidate| entry.requests_authorized || candidate.weight() == highest_weight)
        .collect::<Vec<_>>();
    let request = WireAccountScheduleRequest {
        request_id: input.request_id().as_str().to_owned(),
        attempt_index: input.attempt_index().get(),
        provider: input.provider().as_str().to_owned(),
        model: input.model().map(str::to_owned),
        candidates: visible
            .iter()
            .map(|candidate| WireAccountScheduleCandidate {
                account_id: candidate.account_id().as_str().to_owned(),
                weight: candidate.weight(),
                in_flight: candidate.in_flight(),
                maximum_concurrency: candidate.maximum_concurrency(),
                last_started_at_ms: system_time_millis(candidate.last_started_at()),
                quota_reset_at_ms: system_time_millis(candidate.quota_reset_at()),
                quota_remaining_rank: candidate.quota_remaining_rank(),
                failure_rate_basis_points: candidate.failure_rate_basis_points(),
                first_output_latency_ms: candidate.first_output_latency_ms(),
            })
            .collect(),
    };
    let mut context = invocation.session.context(Stage::Scheduling, timeout);
    context.request_id = Some(input.request_id().as_str().to_owned());
    context.attempt_id = Some(format!(
        "{}:{}",
        input.request_id().as_str(),
        input.attempt_index()
    ));
    let _network_scope = invocation
        .callbacks
        .prepare_data_plane(&context, input.execution_effects())
        .map_err(|_| ())?;
    let reply = invocation
        .session
        .call(
            "policy.schedule_account",
            context,
            serde_json::to_value(request).map_err(|_| ())?,
            Vec::new(),
        )
        .await
        .map_err(|_| ())?;
    if !reply.payload.is_empty() {
        return Err(());
    }
    let decision: WireAccountScheduleDecision =
        serde_json::from_value(reply.result).map_err(|_| ())?;
    match decision {
        WireAccountScheduleDecision::Delegate => Ok(AccountScheduleDecision::Delegate),
        WireAccountScheduleDecision::Reject => Ok(AccountScheduleDecision::Reject),
        WireAccountScheduleDecision::Pick { account_id } => {
            let account_id = ProviderAccountId::new(account_id).map_err(|_| ())?;
            if !visible
                .iter()
                .any(|candidate| candidate.account_id() == &account_id)
            {
                return Err(());
            }
            Ok(AccountScheduleDecision::Pick(account_id))
        }
    }
}

struct OperationProjection<'a> {
    protocol: &'a str,
    body: OperationBody<'a>,
    context: &'a serde_json::Map<String, serde_json::Value>,
}

enum OperationBody<'a> {
    Object(&'a serde_json::Map<String, serde_json::Value>),
    Raw(&'a [u8]),
}

impl OperationBody<'_> {
    fn to_bytes(&self) -> Result<Vec<u8>, ()> {
        match self {
            Self::Raw(body) => Ok(body.to_vec()),
            Self::Object(body) => serde_json::to_vec(body).map_err(|_| ()),
        }
    }
}

fn operation_projection(operation: &Operation) -> Result<OperationProjection<'_>, ()> {
    Ok(match operation {
        Operation::Generate(request) => {
            let payload = request.protocol_payload();
            OperationProjection {
                protocol: payload.protocol(),
                body: OperationBody::Object(payload.body()),
                context: payload.context(),
            }
        }
        Operation::GenerateImage(request) => {
            let payload = request.payload();
            OperationProjection {
                protocol: payload.protocol(),
                body: OperationBody::Raw(payload.body().as_ref()),
                context: payload.context(),
            }
        }
        Operation::Search(request) => {
            let payload = request.payload();
            OperationProjection {
                protocol: payload.protocol(),
                body: OperationBody::Raw(payload.body().as_ref()),
                context: payload.context(),
            }
        }
        Operation::CountTokens(request) => {
            let payload = request.payload();
            OperationProjection {
                protocol: payload.protocol(),
                body: OperationBody::Raw(payload.body().as_ref()),
                context: payload.context(),
            }
        }
        Operation::ProviderHttp(request) => {
            let payload = request.payload();
            OperationProjection {
                protocol: payload.protocol(),
                body: OperationBody::Raw(payload.body().as_ref()),
                context: payload.context(),
            }
        }
        _ => return Err(()),
    })
}

fn project_request_headers(
    operation: &Operation,
    context: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<PolicyHeader>, ()> {
    match operation {
        Operation::ProviderHttp(request) => project_provider_http_headers(request.headers()),
        _ => project_headers(context),
    }
}

fn project_provider_http_headers(headers: &[ProviderHttpHeader]) -> Result<Vec<PolicyHeader>, ()> {
    if headers.len() > MAX_HEADERS {
        return Err(());
    }
    let mut projected = Vec::with_capacity(headers.len());
    let mut total_bytes = 0_usize;
    for header in headers {
        let name = normalized_header_name(header.name())?;
        let value_base64 = BASE64.encode(header.value());
        if value_base64.len() > MAX_HEADER_VALUE_BYTES {
            return Err(());
        }
        total_bytes = total_bytes
            .checked_add(name.len())
            .and_then(|total| total.checked_add(value_base64.len()))
            .ok_or(())?;
        if total_bytes > MAX_HEADER_TOTAL_BYTES {
            return Err(());
        }
        if sensitive_or_identity_header(&name) {
            continue;
        }
        projected.push(PolicyHeader { name, value_base64 });
    }
    Ok(projected)
}

fn project_headers(
    context: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<PolicyHeader>, ()> {
    let Some(value) = context.get(OPAQUE_HEADERS_KEY) else {
        return Ok(Vec::new());
    };
    let entries = value.as_array().ok_or(())?;
    if entries.len() > MAX_HEADERS {
        return Err(());
    }
    let mut projected = Vec::with_capacity(entries.len());
    let mut total_bytes = 0_usize;
    for entry in entries {
        let pair = entry.as_array().filter(|pair| pair.len() == 2).ok_or(())?;
        let name = normalized_header_name(pair[0].as_str().ok_or(())?)?;
        let value_base64 = pair[1].as_str().ok_or(())?;
        if value_base64.len() > MAX_HEADER_VALUE_BYTES
            || value_base64.len() % 4 != 0
            || !value_base64
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        {
            return Err(());
        }
        total_bytes = total_bytes
            .checked_add(name.len())
            .and_then(|total| total.checked_add(value_base64.len()))
            .ok_or(())?;
        if total_bytes > MAX_HEADER_TOTAL_BYTES {
            return Err(());
        }
        if sensitive_or_identity_header(&name) {
            continue;
        }
        projected.push(PolicyHeader {
            name,
            value_base64: value_base64.to_owned(),
        });
    }
    Ok(projected)
}

fn normalized_header_name(name: &str) -> Result<String, ()> {
    let name = name.to_ascii_lowercase();
    if name.is_empty()
        || name.len() > MAX_HEADER_NAME_BYTES
        || !name
            .bytes()
            // HTTP token 允许下划线等字符；合法会话别名仍由后续可见性规则隐藏。
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
    {
        return Err(());
    }
    Ok(name)
}

fn sensitive_or_identity_header(name: &str) -> bool {
    name.contains("auth")
        || name.contains("credential")
        || name.contains("secret")
        || name.contains("token")
        || name.contains("cookie")
        || name.contains("session")
        || name.contains("conversation")
        || name.contains("thread")
        || name.contains("account")
        || name.contains("organization")
        || name.contains("project")
        || name.contains("tenant")
        || name.contains("principal")
        || name.contains("identity")
        || name.contains("user-id")
        || name.ends_with("-key")
        || name.ends_with("_key")
}

fn system_time_millis(value: Option<SystemTime>) -> Option<u64> {
    value?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}
