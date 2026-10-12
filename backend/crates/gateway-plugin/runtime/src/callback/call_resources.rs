//! 单次宿主回调的资源状态、流式预算与统一终结

use super::{CallbackScope, MiddlewareCallback, denied, http_middleware, model};
use gateway_core::lifecycle::CancellationToken;
use gateway_host::outbound::HttpBody;
use gateway_plugin_sdk::{CallContext, ErrorCode, PluginFault, client::CallDeadline};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

pub(super) struct CallResources {
    pub(super) deadline: CallDeadline,
    operation_timeout: Duration,
    pub(super) cancellation: CancellationToken,
    pub(super) scope: Arc<CallbackScope>,
    pub(super) http_resources: Arc<http_middleware::resources::Resources>,
    pub(super) model_bindings: tokio::sync::Mutex<
        BTreeMap<String, gateway_core::engine::execution::BoundModelExecutionContext>,
    >,
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
            cancellation: CancellationToken::new(),
            scope,
            http_resources: middleware
                .as_ref()
                .and_then(|middleware| middleware.http_resources())
                .unwrap_or_default(),
            model_bindings: tokio::sync::Mutex::new(BTreeMap::new()),
            state: Mutex::new(CallState {
                middleware,
                ..CallState::default()
            }),
        }
    }

    pub(super) fn close(&self) {
        self.cancellation.cancel();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
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

    pub(super) fn request_settings(
        &self,
    ) -> Option<gateway_core::routing::request_settings::RequestSettings> {
        let middleware = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .middleware
            .clone();
        middleware.and_then(|middleware| middleware.request_settings())
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
