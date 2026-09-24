use std::{collections::BTreeMap, sync::Arc, time::Duration};

use futures::future::BoxFuture;
use gateway_admin::model::{
    AdminError,
    plugins::instances::{
        PluginCapabilityBinding, PluginFailurePolicy, PluginFrontendIdentityBinding, PluginInstance,
    },
};
use gateway_core::{
    engine::authentication::{
        ClientAuthenticationRequest, FrontendAuthenticationDecision, FrontendAuthenticationError,
        FrontendAuthenticationPlan,
    },
    policy::ClientApiKeyId,
};
use gateway_plugin_sdk::{
    Capability, Manifest, Stage,
    call::frontend_authentication::{
        FrontendAuthenticationIdentifier, FrontendAuthenticationRequest,
        FrontendAuthenticationResult,
    },
};

use crate::{RpcSession, callback::PluginCallbacks};

const IDENTIFIER_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct FrontendAuthenticationEntry {
    identities: BTreeMap<String, ClientApiKeyId>,
    exclusive: bool,
    invocation: Option<(Arc<RpcSession>, Arc<PluginCallbacks>)>,
}

pub(crate) async fn prepare_entry(
    manifest: &Manifest,
    bindings: &[PluginCapabilityBinding],
    session: Arc<RpcSession>,
    callbacks: Arc<PluginCallbacks>,
) -> Result<Option<FrontendAuthenticationEntry>, AdminError> {
    let Some(compiled) = compile_binding(manifest, bindings)? else {
        return Ok(None);
    };
    let reply = session
        .call(
            "frontend_auth.identifier",
            session.context(Stage::Registration, IDENTIFIER_TIMEOUT),
            serde_json::json!({}),
            Vec::new(),
        )
        .await
        .map_err(|_| AdminError::invalid("插件入口认证器注册失败"))?;
    if !reply.payload.is_empty() {
        return Err(AdminError::invalid("插件入口认证器注册结果无效"));
    }
    let identifier: FrontendAuthenticationIdentifier = serde_json::from_value(reply.result)
        .map_err(|_| AdminError::invalid("插件入口认证器注册结果无效"))?;
    if !valid_identifier(&identifier.identifier) {
        return Err(AdminError::invalid("插件入口认证器标识无效"));
    }
    Ok(Some(FrontendAuthenticationEntry {
        identities: compiled.identities,
        exclusive: compiled.binding.failure_policy == PluginFailurePolicy::Reject,
        invocation: Some((session, callbacks)),
    }))
}

/// 认证器恢复失败仍占据认证入口，不能静默改走其他认证路径。
pub(crate) fn unavailable_entry(instance: &PluginInstance) -> Option<FrontendAuthenticationEntry> {
    instance
        .bindings
        .iter()
        .find(|binding| binding.stage == "authentication")
        .map(|binding| FrontendAuthenticationEntry {
            identities: BTreeMap::new(),
            exclusive: binding.failure_policy == PluginFailurePolicy::Reject,
            invocation: None,
        })
}

pub(crate) fn validate_bindings(
    manifest: &Manifest,
    bindings: &[PluginCapabilityBinding],
) -> Result<(), AdminError> {
    compile_binding(manifest, bindings).map(drop)
}

struct CompiledBinding<'a> {
    binding: &'a PluginCapabilityBinding,
    identities: BTreeMap<String, ClientApiKeyId>,
}

fn compile_binding<'a>(
    manifest: &Manifest,
    bindings: &'a [PluginCapabilityBinding],
) -> Result<Option<CompiledBinding<'a>>, AdminError> {
    let mut selected = None;
    for binding in bindings {
        if crate::contribution::resolve(manifest, binding)?.capability
            != Capability::FrontendAuthentication
        {
            continue;
        }
        if selected.replace(binding).is_some() {
            return Err(AdminError::invalid(
                "同一插件实例的入口认证能力只能绑定一次",
            ));
        }
    }
    let Some(binding) = selected else {
        return Ok(None);
    };
    let stage: Stage = serde_json::from_value(serde_json::Value::String(binding.stage.clone()))
        .map_err(|_| AdminError::invalid("插件入口认证阶段无效"))?;
    if stage != Stage::Authentication
        || !matches!(
            &binding.failure_policy,
            PluginFailurePolicy::Reject | PluginFailurePolicy::Delegate
        )
        || !binding.client_key_ids.is_empty()
        || !binding.account_group_ids.is_empty()
        || !binding.provider_ids.is_empty()
        || !binding.models.is_empty()
    {
        return Err(AdminError::invalid(
            "入口认证绑定必须使用 authentication 与 reject/delegate",
        ));
    }
    Ok(Some(CompiledBinding {
        binding,
        identities: compile_identities(&binding.identity_bindings)?,
    }))
}

fn compile_identities(
    bindings: &[PluginFrontendIdentityBinding],
) -> Result<BTreeMap<String, ClientApiKeyId>, AdminError> {
    if bindings.is_empty() || bindings.len() > 256 {
        return Err(AdminError::invalid("入口认证身份映射不能为空或超过上限"));
    }
    let mut identities = BTreeMap::new();
    for binding in bindings {
        if binding.principal.trim() != binding.principal
            || binding.principal.is_empty()
            || binding.principal.len() > 256
            || binding.principal.chars().any(char::is_control)
        {
            return Err(AdminError::invalid("入口认证 principal 无效"));
        }
        let key = ClientApiKeyId::new(binding.client_key_id.clone())
            .map_err(|_| AdminError::invalid("入口认证 Client Key ID 无效"))?;
        if identities.insert(binding.principal.clone(), key).is_some() {
            return Err(AdminError::invalid("入口认证 principal 重复"));
        }
    }
    Ok(identities)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte))
}

pub(crate) struct PluginFrontendAuthenticationPlan {
    entry: FrontendAuthenticationEntry,
    timeout: Duration,
}

impl PluginFrontendAuthenticationPlan {
    pub(crate) fn compile(
        mut entries: Vec<FrontendAuthenticationEntry>,
        maximum_call_timeout: Duration,
    ) -> Result<Option<Arc<Self>>, AdminError> {
        match entries.len() {
            0 => Ok(None),
            1 => Ok(Some(Arc::new(Self {
                entry: entries.pop().expect("one entry was checked"),
                timeout: maximum_call_timeout.min(Duration::from_secs(30)),
            }))),
            _ => Err(AdminError::invalid("同一发布集合只能启用一个入口认证插件")),
        }
    }
}

impl FrontendAuthenticationPlan for PluginFrontendAuthenticationPlan {
    fn authenticate<'a>(
        &'a self,
        request: &'a ClientAuthenticationRequest,
    ) -> BoxFuture<'a, Result<FrontendAuthenticationDecision, FrontendAuthenticationError>> {
        Box::pin(async move {
            let (session, callbacks) = self
                .entry
                .invocation
                .as_ref()
                .ok_or(FrontendAuthenticationError)?;
            let context = session.context(Stage::Authentication, self.timeout);
            let _scope = callbacks
                .prepare_frontend_authentication(&context)
                .map_err(|_| FrontendAuthenticationError)?;
            let input = FrontendAuthenticationRequest {
                authorization: request.authorization().to_owned(),
            };
            let reply = session
                .call(
                    "frontend_auth.authenticate",
                    context,
                    serde_json::json!({}),
                    serde_json::to_vec(&input).map_err(|_| FrontendAuthenticationError)?,
                )
                .await
                .map_err(|_| FrontendAuthenticationError)?;
            if reply.result != serde_json::json!({}) {
                return Err(FrontendAuthenticationError);
            }
            let result: FrontendAuthenticationResult =
                serde_json::from_slice(&reply.payload).map_err(|_| FrontendAuthenticationError)?;
            match result {
                FrontendAuthenticationResult::Authenticated { principal }
                    if valid_principal(&principal) =>
                {
                    Ok(FrontendAuthenticationDecision::Authenticated { principal })
                }
                FrontendAuthenticationResult::Authenticated { .. } => {
                    Err(FrontendAuthenticationError)
                }
                FrontendAuthenticationResult::NotMatched {} => {
                    Ok(FrontendAuthenticationDecision::NotMatched)
                }
                FrontendAuthenticationResult::Rejected {} => {
                    Ok(FrontendAuthenticationDecision::Rejected)
                }
            }
        })
    }

    fn client_key_id(&self, principal: &str) -> Option<ClientApiKeyId> {
        self.entry.identities.get(principal).cloned()
    }

    fn exclusive(&self) -> bool {
        self.entry.exclusive
    }
}

fn valid_principal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
