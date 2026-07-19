//! OpenAI HTTP adapter 与应用组合根之间的窄端口。

use std::{future::Future, pin::Pin};

use async_trait::async_trait;
use gateway_core::{
    engine::{CommitRequirement, EngineError},
    error::GatewayError,
    event::GatewayEvent,
    routing::PublicModelId,
};

use super::{auth::ClientApiKeyAuthError, responses::DecodedResponsesRequest};

/// 由应用生命周期控制器接管的长连接任务。
pub type ConnectionTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Responses 请求使用的下游传输。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsesTransport {
    /// 普通 HTTP 或 HTTP SSE。
    Http,
    /// OpenAI Responses WebSocket。
    WebSocket,
}

/// 一条带下游提交边界的 canonical delivery event。
pub struct DeliveryEvent {
    event: GatewayEvent,
    commit_requirement: CommitRequirement,
}

impl DeliveryEvent {
    /// 绑定 canonical event 与 Engine 已冻结的提交要求。
    #[must_use]
    pub const fn new(event: GatewayEvent, commit_requirement: CommitRequirement) -> Self {
        Self {
            event,
            commit_requirement,
        }
    }

    /// 拆分 canonical event 与提交要求。
    #[must_use]
    pub fn into_parts(self) -> (GatewayEvent, CommitRequirement) {
        (self.event, self.commit_requirement)
    }
}

/// OpenAI response delivery 只依赖的执行会话端口。
#[async_trait]
pub trait ResponseExecutionSession: Send + 'static {
    /// 读取下一条带提交语义的 canonical event。
    async fn next_delivery_event(&mut self) -> Result<Option<DeliveryEvent>, EngineError>;

    /// 在提交下游前收集完整 canonical event 序列。
    async fn collect_uncommitted(&mut self) -> Result<Vec<GatewayEvent>, EngineError>;

    /// 原子提交下游 delivery 边界。
    async fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> Result<(), EngineError>;

    /// 首字节前失败已经终结后，补写协议 adapter 实际返回的 HTTP 状态。
    async fn record_client_status(&mut self, client_status_code: u16) -> Result<(), EngineError>;

    /// 返回执行事实是否已经终结。
    fn is_finalized(&self) -> bool;

    /// 触发请求取消；不得等待清理完成。
    fn cancel(&self);

    /// 将未完成会话交给应用 runtime 做脱离 HTTP 生命周期的最终清理。
    fn detach_finalize(self);
}

/// 已完成认证、请求装配和 logical-request 持久化的执行结果。
pub struct StartedResponse<S> {
    request_id: String,
    session: S,
    created_at_unix_seconds: u64,
    streaming: bool,
}

impl<S> StartedResponse<S> {
    /// 构造协议 adapter 可消费的执行事实。
    #[must_use]
    pub fn new(
        request_id: String,
        session: S,
        created_at_unix_seconds: u64,
        streaming: bool,
    ) -> Self {
        Self {
            request_id,
            session,
            created_at_unix_seconds,
            streaming,
        }
    }

    /// 返回 logical request ID。
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// 拆分 handler 编码所需的冻结事实。
    #[must_use]
    pub fn into_parts(self) -> (S, u64, bool) {
        (self.session, self.created_at_unix_seconds, self.streaming)
    }
}

/// OpenAI 客户端协议所需的应用服务端口。
#[async_trait]
pub trait OpenAiClientService: Clone + Send + Sync + 'static {
    /// 应用内部的已认证调用方快照。
    type Client: Clone + Send + Sync + 'static;
    /// 应用数据面执行会话。
    type Session: ResponseExecutionSession;

    /// 用明文下游 API key 冻结一次认证快照。
    fn authenticate(&self, plaintext: &str) -> Result<Self::Client, ClientApiKeyAuthError>;

    /// 返回当前调用方可见的全部公开模型。
    fn public_models(&self, client: &Self::Client) -> Vec<String>;

    /// 判断调用方是否可访问一个已验证语法的公开模型。
    fn contains_public_model(&self, client: &Self::Client, model: &PublicModelId) -> bool;

    /// 启动唯一数据面执行路径。
    async fn start_response(
        &self,
        client: Self::Client,
        request: DecodedResponsesRequest,
        transport: ResponsesTransport,
    ) -> Result<StartedResponse<Self::Session>, GatewayError>;

    /// 返回进程是否已停止接收新的长连接。
    fn is_shutting_down(&self) -> bool;

    /// 把长连接任务注册到应用生命周期控制器。
    fn spawn_connection(&self, task: ConnectionTask);

    /// 分配仅用于诊断的连接 ID。
    fn next_connection_id(&self) -> String;

    /// 分配在数据面启动前使用的安全请求关联 ID。
    fn next_request_id(&self) -> String;
}

/// Axum state 向协议 router 暴露的唯一组合入口。
pub trait OpenAiApiState: Clone + Send + Sync + 'static {
    /// 每个请求获取的轻量服务 adapter。
    type Service: OpenAiClientService;

    /// 返回绑定同一应用服务集合的 OpenAI adapter。
    fn openai_client_api(&self) -> Self::Service;
}
