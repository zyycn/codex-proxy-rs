//! 插件调用的权限作用域、子调用上下文与出站副作用观测

use std::sync::Arc;

use gateway_core::{
    account::OutboundProxy, engine::nested::ExecutionEffects, upstream::UpstreamSendState,
};
use gateway_plugin_sdk::{CallContext, Stage};

/// 所有中间件共享的父调用身份；不混入协议操作或服务参数
#[derive(Clone)]
pub(crate) struct InvocationContext {
    pub request_id: String,
    pub call_id: String,
    pub cancellation: gateway_core::lifecycle::CancellationToken,
    pub plan: gateway_core::engine::middleware::FrozenMiddlewarePlan,
}

/// 一次回调的调用链、账号与网络事实，由操作持有到完成
pub(crate) struct CallbackScope {
    pub(super) origin: Option<InvocationContext>,
    stage: Stage,
    account_id: Option<String>,
    credential_revision: Option<u64>,
    pub(super) proxy: Option<OutboundProxy>,
    pub(super) extension_scope: gateway_core::engine::extensions::ExtensionCallScope,
    execution_effects: Option<Arc<ExecutionEffects>>,
    pub(super) upstream: Option<Arc<super::upstream::ManagedUpstream>>,
}

impl CallbackScope {
    pub(super) fn observe_http_dispatch(&self) {
        // 子路由及其插件可能执行模型或提交事务，父执行不能再按 Provider 的 not_sent 透明重放
        if let Some(effects) = &self.execution_effects {
            effects.observe();
        }
    }

    pub(super) fn child_extensions(
        &self,
        instance_id: &str,
    ) -> Result<gateway_core::engine::extensions::ExtensionCallScope, gateway_plugin_sdk::PluginFault>
    {
        // 上游适配器在进入回调前已登记当前实例；主动子调用沿用该集合
        if self.extension_scope.contains(instance_id) {
            return Ok(self.extension_scope.clone());
        }
        if self.extension_scope.len()
            >= gateway_core::engine::extensions::ExtensionCallScope::MAXIMUM_DEPTH
        {
            return Err(gateway_plugin_sdk::PluginFault::new(
                gateway_plugin_sdk::ErrorCode::Capacity,
                "child call depth exceeded",
            ));
        }
        self.extension_scope
            .extending(instance_id.to_owned())
            .ok_or_else(super::invalid)
    }

    pub(super) fn new(context: &CallContext, proxy: Option<OutboundProxy>) -> Self {
        Self {
            origin: None,
            stage: context.stage,
            account_id: context.account_id.clone(),
            credential_revision: context.credential_revision,
            proxy,
            extension_scope: Default::default(),
            execution_effects: None,
            upstream: None,
        }
    }

    pub(super) fn for_call(context: &CallContext) -> Self {
        Self::new(context, None)
    }

    pub(super) fn with_execution_effects(mut self, effects: Arc<ExecutionEffects>) -> Self {
        self.execution_effects = Some(effects);
        self
    }

    pub(super) fn with_upstream(mut self, managed: Arc<super::upstream::ManagedUpstream>) -> Self {
        self.execution_effects = managed.effects.clone();
        self.upstream = Some(managed);
        self
    }

    pub(super) fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }

    pub(super) const fn credential_revision(&self) -> Option<u64> {
        self.credential_revision
    }

    pub(super) fn authorizes(&self, context: &CallContext) -> bool {
        context.stage == self.stage
            && context.account_id == self.account_id
            && context.credential_revision == self.credential_revision
    }

    pub(super) fn start_upstream(
        &self,
        purpose: Option<gateway_plugin_sdk::call::upstream_adapter::UpstreamPathPurpose>,
    ) -> HttpAttempt {
        HttpAttempt {
            effects: if purpose
                == Some(gateway_plugin_sdk::call::upstream_adapter::UpstreamPathPurpose::Inference)
            {
                None
            } else {
                self.execution_effects.clone()
            },
            upstream: self
                .upstream
                .as_ref()
                .map(|managed| Arc::clone(&managed.send_state)),
            completed: false,
        }
    }
}

pub(super) struct HttpAttempt {
    effects: Option<Arc<ExecutionEffects>>,
    completed: bool,
    upstream: Option<Arc<super::upstream::SendWatermark>>,
}

impl HttpAttempt {
    pub(super) fn finish(mut self, observed: UpstreamSendState) {
        self.observe(observed);
        self.completed = true;
    }

    fn observe(&self, observed: UpstreamSendState) {
        if let Some(upstream) = &self.upstream {
            upstream.observe(observed);
        }
        if observed != UpstreamSendState::NotSent
            && let Some(effects) = &self.effects
        {
            effects.observe();
        }
    }
}

impl Drop for HttpAttempt {
    fn drop(&mut self) {
        // HTTP future 被取消时无法证明上游未接收；通知 Core 收紧重放判断
        if !self.completed {
            self.observe(UpstreamSendState::Ambiguous);
        }
    }
}
