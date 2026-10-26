//! 单次宿主回调的资源状态、流式预算与统一终结

use super::{CallbackScope, MiddlewareCallback, denied, http_middleware, model};
use gateway_core::lifecycle::CancellationToken;
use gateway_host::outbound::HttpBody;
use gateway_plugin_sdk::{CallContext, ErrorCode, PluginFault, client::CallDeadline};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

pub(super) struct CallResources {
    pub(super) deadline: CallDeadline,
    operation_timeout: Duration,
    pub(super) cancellation: CancellationToken,
    pub(super) scope: Arc<CallbackScope>,
    pub(super) http_resources: Arc<http_middleware::resources::Resources>,
    pub(super) client_identities:
        tokio::sync::Mutex<BTreeMap<String, gateway_core::engine::execution::AuthenticatedClient>>,
    pub(super) state: Mutex<CallState>,
}

impl CallResources {
    pub(super) fn new(
        context: &CallContext,
        deadline: &CallDeadline,
        scope: Arc<CallbackScope>,
        middleware: Option<Arc<dyn MiddlewareCallback>>,
    ) -> Self {
        Self {
            deadline: deadline.clone(),
            operation_timeout: Duration::from_millis(context.timeout_ms),
            cancellation: scope
                .origin
                .as_ref()
                .map_or_else(CancellationToken::new, |origin| {
                    origin.cancellation.child_token()
                }),
            scope,
            http_resources: middleware
                .as_ref()
                .and_then(|middleware| middleware.http_resources())
                .unwrap_or_default(),
            client_identities: tokio::sync::Mutex::new(BTreeMap::new()),
            state: Mutex::new(CallState {
                middleware,
                ..CallState::default()
            }),
        }
    }

    /// 子调用共享受信来源；身份解析与执行准入仍留在各能力 owner
    pub(super) fn child_origin(
        &self,
        context: &CallContext,
    ) -> Result<gateway_core::engine::extensions::InvocationOrigin, PluginFault> {
        let parent = self.scope.origin.as_ref();
        Ok(gateway_core::engine::extensions::InvocationOrigin {
            request_id: parent.map_or_else(
                || {
                    context
                        .request_id
                        .clone()
                        .unwrap_or_else(|| context.resource_scope_id.clone())
                },
                |origin| origin.request_id.clone(),
            ),
            call_id: parent.map_or_else(
                || format!("{}:{}", context.resource_scope_id, context.call_id),
                |origin| origin.call_id.clone(),
            ),
            cancellation: self.cancellation.child_token(),
            extensions: if self.scope.extension_scope.contains(&context.instance_id) {
                self.scope.extension_scope.clone()
            } else {
                self.scope
                    .extension_scope
                    .extending(context.instance_id.clone())
                    .ok_or_else(super::invalid)?
            },
            plan: parent.and_then(|origin| origin.plan.clone()),
            effects: self.scope.execution_effects.clone(),
        })
    }

    pub(super) fn close(&self) {
        self.cancellation.cancel();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        // abort 只安排回调 future 稍后析构；返回 Core 前同步封存仍在发送的请求
        for attempt in state
            .http_attempts
            .drain(..)
            .filter_map(|attempt| attempt.upgrade())
        {
            attempt.finish(gateway_core::upstream::UpstreamSendState::Ambiguous);
        }
        // RPC End 可以先于消费者取完已入队正文到达，这里只撤销新 callback
        // 正文包装器持有 invocation，最后一个 owner 释放时再关闭，保留计量和终态信封
        state.middleware.take();
        for stream in state.streams.values() {
            stream.close();
        }
        state.streams.clear();
        for stream in state.model_streams.values() {
            stream.close();
        }
        state.model_streams.clear();
    }

    pub(super) fn start_upstream(
        &self,
        purpose: Option<gateway_plugin_sdk::call::upstream_adapter::UpstreamPathPurpose>,
    ) -> Result<super::scope::HttpAttempt, PluginFault> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(denied());
        }
        let attempt = self.scope.start_upstream(purpose);
        state
            .http_attempts
            .retain(|attempt| attempt.strong_count() != 0);
        state.http_attempts.push(attempt.pending());
        Ok(attempt)
    }

    pub(super) fn observe_dispatch(&self) -> Result<(), PluginFault> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(denied());
        }
        self.scope.observe_dispatch();
        Ok(())
    }

    pub(super) fn request_settings(
        &self,
    ) -> Option<gateway_core::routing::request_settings::RequestSettings> {
        let middleware = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .middleware
            .clone();
        middleware
            .and_then(|middleware| middleware.request_settings())
            .or_else(|| self.scope.settings.clone())
    }

    /// 建立受管流后，网络操作各自计时；连接空闲时间不消耗后续操作的预算
    pub(super) fn timeout(&self) -> Result<Duration, PluginFault> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(denied());
        }
        let remaining = self.deadline.remaining().unwrap_or(self.operation_timeout);
        if remaining.is_zero() {
            return Err(PluginFault::new(
                ErrorCode::Timeout,
                "callback deadline elapsed",
            ));
        }
        Ok(remaining)
    }
}

#[derive(Default)]
pub(super) struct CallState {
    pub(super) closed: bool,
    pub(super) middleware: Option<Arc<dyn MiddlewareCallback>>,
    pub(super) streams: BTreeMap<String, Arc<HttpStream>>,
    pub(super) model_streams: BTreeMap<String, Arc<model::ModelStream>>,
    http_attempts: Vec<Weak<super::scope::HttpAttemptObservation>>,
}

pub(super) struct HttpStream {
    pub(super) body: tokio::sync::Mutex<Option<HttpBody>>,
    pub(super) closed: tokio::sync::watch::Sender<bool>,
}

impl HttpStream {
    pub(super) fn close(&self) {
        self.closed.send_replace(true);
        if let Ok(mut body) = self.body.try_lock() {
            body.take();
        }
    }
}
