//! RPC 会话的容量、错误、回调与生命周期合同

use std::{sync::Arc, time::Duration};

use futures::future::BoxFuture;
use gateway_host::process::ProcessStartError;
use gateway_plugin_sdk::{CallBudgetError, CallContext, PluginFault, Stage, client::CallDeadline};

#[derive(Debug, Clone, Copy)]
pub struct RpcLimits {
    /// 受管回调与观察队列的缓冲预算，不是请求正文或 IPC 消息总量上限
    pub maximum_buffered_body_bytes: usize,
    /// 包含初次排队、下游等待、取消确认和未消费流的总在途上限
    pub maximum_calls: usize,
    pub maximum_executing_calls: usize,
    pub maximum_queued_calls: usize,
    pub maximum_queue_timeout: Duration,
    pub maximum_callbacks: usize,
    pub handshake_timeout: Duration,
    pub maximum_stderr_bytes: usize,
}

impl Default for RpcLimits {
    fn default() -> Self {
        Self {
            maximum_buffered_body_bytes: 1024 * 1024,
            maximum_calls: 256,
            maximum_executing_calls: 64,
            maximum_queued_calls: 64,
            maximum_queue_timeout: Duration::from_secs(30),
            maximum_callbacks: 320,
            handshake_timeout: Duration::from_secs(5),
            maximum_stderr_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RpcError {
    #[error("plugin process could not be started: {0}")]
    Start(#[source] ProcessStartError),
    #[error("plugin handshake does not match the prepared instance")]
    Handshake(#[source] Option<gateway_core::error::ErrorSource>),
    #[error("plugin call budget negotiation failed: {0}")]
    Budget(#[from] CallBudgetError),
    #[error("plugin protocol is invalid")]
    Protocol(#[source] Option<gateway_core::error::ErrorSource>),
    #[error("plugin returned an invalid response at {0:?}")]
    InvalidResponse(Stage, #[source] Option<gateway_core::error::ErrorSource>),
    #[error("plugin process or transport has stopped")]
    Closed(#[source] Option<gateway_core::error::ErrorSource>),
    #[error("plugin call exceeded its deadline")]
    Timeout,
    #[error("plugin session was cancelled")]
    Cancelled,
    #[error("plugin call capacity is exhausted")]
    Capacity,
    #[error("plugin admission capacity is exhausted")]
    Overloaded,
    #[error("plugin call context is invalid")]
    Context(#[source] Option<gateway_core::error::ErrorSource>),
    #[error("plugin returned an error")]
    Remote(PluginFault),
}

impl RpcError {
    pub(crate) fn is_overloaded(&self) -> bool {
        matches!(self, Self::Overloaded)
            || matches!(self, Self::Remote(fault) if fault.code == gateway_plugin_sdk::ErrorCode::Capacity)
    }

    /// 会话状态与请求错误共用安全分类，不展开插件返回正文
    pub(crate) fn diagnostic(&self) -> (&'static str, &'static str) {
        match self {
            RpcError::Start(_) => ("process_start", "插件进程启动失败"),
            RpcError::Handshake(_) => ("handshake", "插件握手失败"),
            RpcError::Budget(_) => ("call_budget", "插件调用预算协商失败"),
            RpcError::Protocol(_) => ("protocol", "插件协议错误"),
            RpcError::InvalidResponse(stage, _) => (
                "invalid_response",
                match stage {
                    Stage::Registration => "注册阶段：插件返回了无效响应",
                    Stage::Configuration => "配置阶段：插件返回了无效响应",
                    Stage::Authentication => "认证阶段：插件返回了无效响应",
                    Stage::Routing => "路由阶段：插件返回了无效响应",
                    Stage::Scheduling => "调度阶段：插件返回了无效响应",
                    Stage::Retry => "重试阶段：插件返回了无效响应",
                    Stage::Http => "HTTP 中间件：插件返回了无效响应",
                    Stage::Service => "服务中间件：插件返回了无效响应",
                    Stage::WebSocket => "WebSocket 中间件：插件返回了无效响应",
                    Stage::Request => "请求中间件：插件返回了无效响应",
                    Stage::Attempt => "尝试中间件：插件返回了无效响应",
                    Stage::Outbound => "出站准备中间件：插件返回了无效响应",
                    Stage::Upstream => "上游适配阶段：插件返回了无效响应",
                    Stage::Observation => "观察阶段：插件返回了无效响应",
                    Stage::Management | Stage::PublicManagement => "管理阶段：插件返回了无效响应",
                    Stage::CommandLine => "命令行阶段：插件返回了无效响应",
                    Stage::Maintenance => "维护阶段：插件返回了无效响应",
                },
            ),
            RpcError::Closed(_) => ("closed", "插件进程或传输已停止"),
            RpcError::Timeout => ("timeout", "插件调用超时"),
            RpcError::Cancelled => ("cancelled", "插件会话已取消"),
            RpcError::Capacity => ("capacity", "插件调用容量已耗尽"),
            RpcError::Overloaded => ("overloaded", "插件暂时繁忙"),
            RpcError::Context(_) => ("context", "插件调用上下文无效"),
            RpcError::Remote(_) => ("remote", "插件返回错误"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RpcSessionDiagnostic {
    Ready,
    Quiescing,
    Failed {
        code: &'static str,
        message: &'static str,
    },
}

pub(crate) struct RpcSessionLifecycle {
    pub(super) failure: Arc<dyn Fn(Duration) + Send + Sync>,
    end: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl RpcSessionLifecycle {
    pub(crate) fn new(
        failure: impl Fn(Duration) + Send + Sync + 'static,
        end: impl FnOnce() + Send + Sync + 'static,
    ) -> Self {
        Self {
            failure: Arc::new(failure),
            end: Some(Box::new(end)),
        }
    }

    pub(super) fn fail(&self, uptime: Duration) {
        (self.failure)(uptime);
    }
}

/// 从实际租约与调用表采样，各项存在重叠，不构成同一时刻的可加总分区
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RpcSessionLoad {
    /// 总在途租约，包含初次排队、取消确认和 End 后仍由消费者持有的流
    pub(crate) active_calls: usize,
    /// 正在占用执行名额的调用，包含取消确认前尚未归还的名额
    pub(crate) executing_calls: usize,
    /// 正在占用初次准入排队名额的调用
    pub(crate) queued_calls: usize,
    /// 调用表中等待恢复授权的调用
    pub(crate) resuming_calls: usize,
    /// 驻留回调名额，包含已回复但插件尚未消费的结果
    pub(crate) pending_callbacks: usize,
    /// 已交付首结果且尚未收到 End 或取消的协议响应流
    pub(crate) active_streams: usize,
}

impl Drop for RpcSessionLifecycle {
    fn drop(&mut self) {
        if let Some(end) = self.end.take() {
            end();
        }
    }
}

/// 不派生 Debug，防止原始请求、凭据或模型输出进入宿主诊断
pub struct RpcReply {
    pub result: serde_json::Value,
    pub payload: Vec<u8>,
}

/// 具体回调在领域适配器校验账号、出站与执行身份；上下文只取自宿主在途调用表
pub trait CallbackHandler: Send + Sync {
    /// 在 Call 可见之前建立资源范围；不得执行 I/O 或调用 RPC
    fn begin(&self, _context: &CallContext, _deadline: &CallDeadline) {}

    /// 调用结束或取消时立即撤销其句柄；实现必须幂等且不能阻塞
    fn finish(&self, _context: &CallContext) {}

    /// 首个流式结果已到达；资源仍绑定原调用并在终态统一回收
    fn streaming(&self, _context: &CallContext) {}

    fn call(
        &self,
        context: CallContext,
        method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>>;
}
