//! 编译各挂载边界的中间件绑定，组合为独立执行计划

mod execution;
mod http;
mod outbound;
mod request;
mod service;
mod websocket;

use crate::{RpcSession, adapter::scope::BindingScope, callback::PluginCallbacks};
use gateway_admin::model::{
    AdminError,
    plugins::instances::{PluginCapabilityBinding, PluginFailurePolicy, PluginInstance},
};
use gateway_core::engine::middleware::MiddlewareMount;
use gateway_plugin_sdk::{Capability, Manifest, Stage};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
    time::Duration,
};

#[derive(Clone)]
struct MiddlewareInvocationPorts {
    session: Arc<RpcSession>,
    callbacks: Arc<PluginCallbacks>,
}

#[derive(Clone)]
pub(crate) struct MiddlewareEntry {
    order: i32,
    plugin_id: String,
    instance_id: String,
    mount: MiddlewareMount,
    invocation: Option<MiddlewareInvocationPorts>,
    scope: BindingScope,
    failure_policy: PluginFailurePolicy,
}

pub(crate) fn validate_bindings(
    manifest: &Manifest,
    bindings: &[PluginCapabilityBinding],
) -> Result<(), AdminError> {
    let mut stages = BTreeSet::new();
    for binding in bindings {
        if crate::contribution::resolve(manifest, binding)?.capability != Capability::Middleware {
            continue;
        }
        let stage: Stage = serde_json::from_value(serde_json::Value::String(binding.stage.clone()))
            .map_err(|_| AdminError::invalid("插件中间件阶段无效"))?;
        if !matches!(
            binding.failure_policy,
            PluginFailurePolicy::Reject | PluginFailurePolicy::Delegate
        ) {
            return Err(AdminError::invalid("数据面绑定的故障策略无效"));
        }
        if !matches!(
            stage,
            Stage::Http
                | Stage::WebSocket
                | Stage::Service
                | Stage::Request
                | Stage::Attempt
                | Stage::Outbound
        ) || !stages.insert(stage)
        {
            return Err(AdminError::invalid(
                "同一插件实例的中间件只能在 http/websocket/service/request/attempt/outbound 各绑定一次",
            ));
        }
        if matches!(stage, Stage::Http | Stage::WebSocket | Stage::Service)
            && (!binding.client_key_ids.is_empty()
                || !binding.account_group_ids.is_empty()
                || !binding.provider_ids.is_empty()
                || !binding.models.is_empty())
        {
            return Err(AdminError::invalid(
                "入口和公开服务不保证具有模型执行身份，不能绑定 Key、分组、Provider 或模型条件",
            ));
        }
        if BindingScope::compile(binding)?.has_provider_condition() && stage == Stage::Request {
            return Err(AdminError::invalid(
                "Provider 尚未冻结的阶段不能绑定 Provider 条件",
            ));
        }
    }
    Ok(())
}

pub(crate) fn compile_entries(
    manifest: &Manifest,
    instance_id: &str,
    bindings: &[PluginCapabilityBinding],
    session: Arc<RpcSession>,
    callbacks: Arc<PluginCallbacks>,
) -> Result<Vec<MiddlewareEntry>, AdminError> {
    validate_bindings(manifest, bindings)?;
    let plugin_id = manifest
        .plugin_id()
        .map_err(|_| AdminError::invalid("插件身份无效"))?;
    let invocation = MiddlewareInvocationPorts { session, callbacks };
    bindings
        .iter()
        .filter_map(|binding| {
            let capability = match crate::contribution::resolve(manifest, binding) {
                Ok(contribution) => contribution.capability,
                Err(error) => return Some(Err(error)),
            };
            (capability == Capability::Middleware)
                .then(|| compile_entry(&plugin_id, instance_id, binding, Some(invocation.clone())))
        })
        .collect()
}

/// 进程不可用时保留原绑定的范围与故障策略
pub(crate) fn unavailable_entries(
    instance: &PluginInstance,
) -> Result<Vec<MiddlewareEntry>, AdminError> {
    instance
        .bindings
        .iter()
        .filter(|binding| {
            matches!(
                binding.stage.as_str(),
                "http" | "websocket" | "service" | "request" | "attempt" | "outbound"
            )
        })
        .map(|binding| {
            compile_entry(
                binding
                    .contribution
                    .rsplit_once('.')
                    .map_or(binding.contribution.as_str(), |(plugin, _)| plugin),
                &instance.id,
                binding,
                None,
            )
        })
        .collect()
}

fn compile_entry(
    plugin_id: &str,
    instance_id: &str,
    binding: &PluginCapabilityBinding,
    invocation: Option<MiddlewareInvocationPorts>,
) -> Result<MiddlewareEntry, AdminError> {
    let scope = BindingScope::compile(binding)?;
    Ok(MiddlewareEntry {
        order: binding.order,
        plugin_id: plugin_id.to_owned(),
        instance_id: instance_id.to_owned(),
        mount: match binding.stage.as_str() {
            "http" => MiddlewareMount::Http,
            "service" => MiddlewareMount::Service,
            "websocket" => MiddlewareMount::WebSocket,
            "request" => MiddlewareMount::Request,
            "attempt" => MiddlewareMount::Attempt,
            "outbound" => MiddlewareMount::Outbound,
            _ => return Err(AdminError::invalid("插件中间件阶段无效")),
        },
        invocation,
        scope,
        failure_policy: binding.failure_policy.clone(),
    })
}

pub(crate) struct PluginMiddlewarePlan {
    middleware: BTreeMap<MiddlewareMount, Box<[Arc<MiddlewareEntry>]>>,
    middleware_timeout: Duration,
}

impl fmt::Debug for PluginMiddlewarePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginMiddlewarePlan")
            .field("mounts", &self.middleware.keys())
            .finish_non_exhaustive()
    }
}

impl PluginMiddlewarePlan {
    pub(crate) fn compile(
        mut entries: Vec<MiddlewareEntry>,
        maximum_call_timeout: Duration,
    ) -> Option<Arc<Self>> {
        if entries.is_empty() {
            return None;
        }
        entries.sort_by(|left, right| {
            (left.order, &left.plugin_id, &left.instance_id).cmp(&(
                right.order,
                &right.plugin_id,
                &right.instance_id,
            ))
        });
        // 发布时固定各挂载位置的有序候选，消息处理不再扫描其他边界的绑定
        let mut by_mount = BTreeMap::<_, Vec<_>>::new();
        for entry in entries {
            by_mount
                .entry(entry.mount)
                .or_default()
                .push(Arc::new(entry));
        }
        Some(Arc::new(Self {
            middleware: by_mount
                .into_iter()
                .map(|(mount, entries)| (mount, entries.into_boxed_slice()))
                .collect(),
            middleware_timeout: maximum_call_timeout,
        }))
    }
}
