//! 编译模型路由、账号调度与重试策略的请求级计划

mod retry;
mod route_schedule;

use std::{fmt, sync::Arc};

use gateway_admin::model::{
    AdminError,
    plugins::instances::{PluginCapabilityBinding, PluginFailurePolicy, PluginInstance},
};
use gateway_plugin_sdk::{Capability, Manifest, Stage};

use crate::{RpcSession, adapter::scope::BindingScope, callback::PluginCallbacks};

#[derive(Clone)]
pub(crate) enum PolicyEntry {
    Router(ModelRouterEntry),
    Scheduler(AccountSchedulerEntry),
    Retry(RetryEntry),
}

#[derive(Clone)]
struct PolicyInvocation {
    session: Arc<RpcSession>,
    callbacks: Arc<PluginCallbacks>,
}

#[derive(Clone)]
pub(crate) struct RetryEntry {
    order: i32,
    plugin_id: String,
    instance_id: String,
    invocation: Option<PolicyInvocation>,
    scope: BindingScope,
}

#[derive(Clone)]
pub(crate) struct ModelRouterEntry {
    order: i32,
    plugin_id: String,
    instance_id: String,
    invocation: Option<PolicyInvocation>,
    scope: BindingScope,
    failure_policy: PluginFailurePolicy,
}

#[derive(Clone)]
pub(crate) struct AccountSchedulerEntry {
    plugin_id: String,
    instance_id: String,
    invocation: Option<PolicyInvocation>,
    scope: BindingScope,
    failure_policy: PluginFailurePolicy,
}

pub(crate) fn validate_bindings(
    manifest: &Manifest,
    bindings: &[PluginCapabilityBinding],
) -> Result<(), AdminError> {
    let mut has_router = false;
    let mut has_scheduler = false;
    let mut has_retry = false;
    for binding in bindings {
        let capability = crate::contribution::resolve(manifest, binding)?.capability;
        if !matches!(
            capability,
            Capability::ModelRouter | Capability::Scheduler | Capability::RetryPolicy
        ) {
            continue;
        }
        let stage: Stage = serde_json::from_value(serde_json::Value::String(binding.stage.clone()))
            .map_err(|_| AdminError::invalid("插件请求阶段无效"))?;
        if !matches!(
            binding.failure_policy,
            PluginFailurePolicy::Reject | PluginFailurePolicy::Delegate
        ) {
            return Err(AdminError::invalid("数据面绑定的故障策略无效"));
        }
        match capability {
            Capability::RetryPolicy => {
                if std::mem::replace(&mut has_retry, true)
                    || stage != Stage::Retry
                    || binding.failure_policy != PluginFailurePolicy::Delegate
                {
                    return Err(AdminError::invalid(
                        "重试策略只允许一次 retry 绑定，故障策略必须为 delegate",
                    ));
                }
            }
            Capability::ModelRouter => {
                if std::mem::replace(&mut has_router, true) || stage != Stage::Routing {
                    return Err(AdminError::invalid(
                        "同一插件实例的模型路由能力只能绑定一次且必须使用 routing 阶段",
                    ));
                }
            }
            Capability::Scheduler => {
                if std::mem::replace(&mut has_scheduler, true) || stage != Stage::Scheduling {
                    return Err(AdminError::invalid(
                        "同一插件实例的账号调度能力只能绑定一次且必须使用 scheduling 阶段",
                    ));
                }
            }
            _ => unreachable!("capability was filtered above"),
        }
        let scope = BindingScope::compile(binding)?;
        if capability == Capability::ModelRouter && scope.has_provider_condition() {
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
) -> Result<Vec<PolicyEntry>, AdminError> {
    validate_bindings(manifest, bindings)?;
    let plugin_id = manifest
        .plugin_id()
        .map_err(|_| AdminError::invalid("插件身份无效"))?;
    let invocation = PolicyInvocation { session, callbacks };
    bindings
        .iter()
        .filter_map(|binding| {
            let capability = match crate::contribution::resolve(manifest, binding) {
                Ok(contribution) => contribution.capability,
                Err(error) => return Some(Err(error)),
            };
            matches!(
                capability,
                Capability::ModelRouter | Capability::Scheduler | Capability::RetryPolicy
            )
            .then(|| {
                compile_entry(
                    &plugin_id,
                    instance_id,
                    binding,
                    capability,
                    Some(invocation.clone()),
                )
            })
        })
        .collect()
}

/// 已保存绑定的阶段是宿主合同；进程不可用时仍保留相同范围和故障策略
pub(crate) fn unavailable_entries(
    instance: &PluginInstance,
) -> Result<Vec<PolicyEntry>, AdminError> {
    instance
        .bindings
        .iter()
        .filter_map(|binding| {
            let capability = match binding.stage.as_str() {
                "routing" => Capability::ModelRouter,
                "scheduling" => Capability::Scheduler,
                "retry" => Capability::RetryPolicy,
                _ => return None,
            };
            Some(compile_entry(
                binding
                    .contribution
                    .rsplit_once('.')
                    .map_or(binding.contribution.as_str(), |(plugin, _)| plugin),
                &instance.id,
                binding,
                capability,
                None,
            ))
        })
        .collect()
}

fn compile_entry(
    plugin_id: &str,
    instance_id: &str,
    binding: &PluginCapabilityBinding,
    capability: Capability,
    invocation: Option<PolicyInvocation>,
) -> Result<PolicyEntry, AdminError> {
    let scope = BindingScope::compile(binding)?;
    Ok(match capability {
        Capability::RetryPolicy => PolicyEntry::Retry(RetryEntry {
            order: binding.order,
            plugin_id: plugin_id.to_owned(),
            instance_id: instance_id.to_owned(),
            invocation,
            scope,
        }),
        Capability::ModelRouter => PolicyEntry::Router(ModelRouterEntry {
            order: binding.order,
            plugin_id: plugin_id.to_owned(),
            instance_id: instance_id.to_owned(),
            invocation,
            scope,
            failure_policy: binding.failure_policy.clone(),
        }),
        Capability::Scheduler => PolicyEntry::Scheduler(AccountSchedulerEntry {
            plugin_id: plugin_id.to_owned(),
            instance_id: instance_id.to_owned(),
            invocation,
            scope,
            failure_policy: binding.failure_policy.clone(),
        }),
        _ => return Err(AdminError::invalid("插件请求策略能力无效")),
    })
}

pub(crate) struct PluginRequestPolicyPlan {
    routers: Arc<[ModelRouterEntry]>,
    schedulers: Arc<[AccountSchedulerEntry]>,
    retries: Arc<[RetryEntry]>,
}

impl fmt::Debug for PluginRequestPolicyPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginRequestPolicyPlan")
            .field("router_count", &self.routers.len())
            .field("scheduler_count", &self.schedulers.len())
            .field("retry_count", &self.retries.len())
            .finish_non_exhaustive()
    }
}

impl PluginRequestPolicyPlan {
    pub(crate) fn compile(entries: Vec<PolicyEntry>) -> Result<Option<Arc<Self>>, AdminError> {
        let mut routers = Vec::new();
        let mut schedulers = Vec::new();
        let mut retries = Vec::new();
        for entry in entries {
            match entry {
                PolicyEntry::Router(entry) => routers.push(entry),
                PolicyEntry::Scheduler(entry) => schedulers.push(entry),
                PolicyEntry::Retry(entry) => retries.push(entry),
            }
        }
        if routers.is_empty() && schedulers.is_empty() && retries.is_empty() {
            return Ok(None);
        }
        routers.sort_by(|left, right| {
            (left.order, &left.plugin_id, &left.instance_id).cmp(&(
                right.order,
                &right.plugin_id,
                &right.instance_id,
            ))
        });
        retries.sort_by(|left, right| {
            (left.order, &left.plugin_id, &left.instance_id).cmp(&(
                right.order,
                &right.plugin_id,
                &right.instance_id,
            ))
        });
        for (index, left) in schedulers.iter().enumerate() {
            if schedulers[index + 1..]
                .iter()
                .any(|right| left.scope.overlaps(&right.scope))
            {
                return Err(AdminError::invalid("账号调度绑定作用范围重叠"));
            }
        }
        Ok(Some(Arc::new(Self {
            routers: routers.into(),
            schedulers: schedulers.into(),
            retries: retries.into(),
        })))
    }
}
