//! 插件管理授权回调的临时状态签发、绑定校验与调用分派

use std::time::Duration;

use chrono::Utc;
use gateway_admin::model::{
    AdminError,
    auth::{AdminPrincipal, AdminRequestContext},
    plugins::management::{
        PluginManagementCallbackTicket, PluginManagementRequest, PluginManagementResponse,
        PluginManagementTarget, StartPluginManagementCallback,
    },
};
use gateway_core::{
    account::OpaqueProviderData,
    provider_ports::{
        NewOAuthPendingFlow, OAuthPendingBinding, OAuthPendingClaimOutcome,
        OAuthPendingConsumeOutcome, OAuthPendingFlowPort, OAuthPendingPutOutcome,
    },
    routing::ProviderKind,
};
use gateway_plugin_sdk::{Stage, call::management::ManagementRequest};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::ManagementEntry;
use crate::RpcLimits;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallbackState {
    target: PluginManagementTarget,
    path: String,
    expires_at_ms: i64,
    owner_digest: String,
}

impl ManagementEntry {
    pub(crate) async fn start_callback(
        &self,
        store: &dyn OAuthPendingFlowPort,
        command: StartPluginManagementCallback,
        context: &AdminRequestContext,
    ) -> Result<PluginManagementCallbackTicket, AdminError> {
        if !(1..=600).contains(&command.ttl_seconds)
            || !self
                .view
                .callbacks
                .iter()
                .any(|callback| callback.path == command.path)
        {
            return Err(AdminError::invalid(
                "插件登录回调未注册或期限不在 1 至 600 秒内",
            ));
        }
        let flow = uuid::Uuid::new_v4().simple().to_string();
        let mut owner = Sha256::new();
        // state 中的 owner 为随机且不可反查管理员的摘要；实际身份仅来自已验证的 Admin 上下文
        owner.update(uuid::Uuid::new_v4().as_bytes());
        match &context.principal {
            AdminPrincipal::Session { admin_user_id } => {
                owner.update(b"admin-session:");
                owner.update(admin_user_id.as_bytes());
            }
            AdminPrincipal::ApiKey => owner.update(b"admin-api-key"),
            AdminPrincipal::Plugin { instance_id } => {
                owner.update(b"plugin:");
                owner.update(instance_id.as_bytes());
            }
        }
        owner.update(self.view.target.instance_id.as_bytes());
        owner.update(command.path.as_bytes());
        let owner = hex::encode(owner.finalize());
        let expires_at_ms = Utc::now().timestamp_millis() + i64::from(command.ttl_seconds) * 1000;
        let payload = serde_json::to_value(CallbackState {
            target: self.view.target.clone(),
            path: command.path,
            expires_at_ms,
            owner_digest: owner.clone(),
        })
        .map_err(|_| unavailable())?;
        let payload = serde_json::from_value(payload).map_err(|_| unavailable())?;
        let pending = NewOAuthPendingFlow::try_new(
            namespace(&self.view.target)?,
            binding(&flow)?,
            binding(&owner)?,
            Duration::from_secs(u64::from(command.ttl_seconds)),
            OpaqueProviderData::new(payload),
        )
        .map_err(|_| unavailable())?;
        if store
            .put_if_absent(pending)
            .await
            .map_err(|_| unavailable())?
            != OAuthPendingPutOutcome::Stored
        {
            return Err(unavailable());
        }
        Ok(PluginManagementCallbackTicket {
            state: format!("{flow}.{owner}"),
            expires_at_ms,
        })
    }

    pub(crate) async fn callback(
        &self,
        store: &dyn OAuthPendingFlowPort,
        state: &str,
        request: PluginManagementRequest,
        limits: RpcLimits,
    ) -> Result<PluginManagementResponse, AdminError> {
        let descriptor = self
            .view
            .callbacks
            .iter()
            .find(|callback| callback.path == request.path)
            .ok_or_else(invalid_state)?;
        if request.method != "GET"
            || !request.body.is_empty()
            || request.content_type.is_some()
            || request.query.len() > 8192
            || request.query.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(AdminError::invalid("插件登录回调请求无效"));
        }
        let (flow, owner) = state
            .split_once('.')
            .filter(|(flow, owner)| {
                flow.len() == 32
                    && owner.len() == 64
                    && flow
                        .bytes()
                        .chain(owner.bytes())
                        .all(|byte| byte.is_ascii_hexdigit())
            })
            .ok_or_else(invalid_state)?;
        let flow = binding(flow)?;
        let owner = binding(owner)?;
        let claim = binding(&uuid::Uuid::new_v4().simple().to_string())?;
        let namespace = namespace(&self.view.target)?;
        let payload = match store
            .claim_if_owner(&namespace, &flow, &owner, &claim, Duration::from_secs(30))
            .await
            .map_err(|_| unavailable())?
        {
            OAuthPendingClaimOutcome::Claimed(payload) => payload,
            _ => return Err(invalid_state()),
        };
        // 在任何插件执行之前消耗 state；崩溃或响应丢失不得重放登录回调
        if store
            .consume_claim(&namespace, &flow, &owner, &claim)
            .await
            .map_err(|_| unavailable())?
            != OAuthPendingConsumeOutcome::Consumed
        {
            return Err(invalid_state());
        }
        let stored: CallbackState =
            serde_json::from_value(serde_json::Value::Object(payload.into_inner()))
                .map_err(|_| invalid_state())?;
        if stored.target != self.view.target
            || stored.path != request.path
            || stored.expires_at_ms <= Utc::now().timestamp_millis()
            || stored.owner_digest != owner.expose_to_store()
        {
            return Err(invalid_state());
        }
        let mut context = self
            .session
            .context(Stage::PublicManagement, limits.maximum_call_timeout);
        context.request_id = Some(request.request_id);
        let params = serde_json::to_value(ManagementRequest {
            method: request.method,
            path: request.path,
            query: request.query,
            content_type: None,
            headers: request
                .headers
                .iter()
                .map(
                    |header| gateway_plugin_sdk::call::middleware::MiddlewareHeader {
                        name: header.name().to_owned(),
                        value: header.value().to_vec(),
                    },
                )
                .collect(),
        })
        .map_err(|_| unavailable())?;
        let reply = self
            .session
            .call("management.callback", context, params, Vec::new())
            .await
            .map_err(|_| {
                AdminError::unavailable("插件登录回调未完成；state 已消费，请重新发起登录")
            })?;
        super::decode_response(reply, &descriptor.response_content_types, limits)
    }
}

fn namespace(target: &PluginManagementTarget) -> Result<ProviderKind, AdminError> {
    ProviderKind::new(format!("plugin-management-{}", target.instance_id))
        .map_err(|_| unavailable())
}

fn binding(value: &str) -> Result<OAuthPendingBinding, AdminError> {
    OAuthPendingBinding::try_new(value).map_err(|_| invalid_state())
}

fn invalid_state() -> AdminError {
    AdminError::invalid("插件登录 state 无效、已过期或已消费")
}
fn unavailable() -> AdminError {
    AdminError::unavailable("插件登录回调状态暂不可用")
}
