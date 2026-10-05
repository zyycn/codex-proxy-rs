//! 主动服务调用的统一组合入口；普通业务方法由所属 HTTP 或执行边界包裹

mod registry;
mod settings;
pub use registry::Registry;

use std::{future::Future, sync::Arc};

use futures::future::BoxFuture;
use gateway_core::{
    engine::middleware::FrozenMiddlewarePlan,
    middleware::{
        compose,
        service::{Context, Error, Value},
    },
};

use crate::model::{AdminError, AdminErrorKind};

pub type PlanSource = Arc<dyn Fn() -> Option<FrozenMiddlewarePlan> + Send + Sync>;

/// 空计划也是快照，只有未解析的外部入口才读取当前发布集合
#[derive(Clone, Default)]
pub enum Plan {
    #[default]
    Current,
    Frozen(Option<FrozenMiddlewarePlan>),
}

/// 非服务入口也有父调用身份和取消信号；发布计划在首次进入公开服务时解析
#[derive(Clone)]
pub struct Origin {
    pub request_id: String,
    pub call_id: String,
    pub cancellation: gateway_core::lifecycle::CancellationToken,
    pub extensions: gateway_core::engine::extensions::ExtensionCallScope,
    pub plan: Plan,
}

impl From<Context> for Origin {
    fn from(context: Context) -> Self {
        Self {
            request_id: context.request_id,
            call_id: context.call_id,
            cancellation: context.cancellation,
            extensions: context.extensions,
            plan: Plan::Frozen(Some(context.plan)),
        }
    }
}

tokio::task_local! { static CURRENT: Origin; }

/// task local 随 future poll 进入/退出，不泄漏给并行任务
pub async fn scope<T>(context: impl Into<Origin>, future: impl Future<Output = T>) -> T {
    CURRENT.scope(context.into(), future).await
}

pub(crate) async fn invoke(
    source: &PlanSource,
    operation: &'static str,
    input: Value,
    terminal: impl FnOnce(Value) -> BoxFuture<'static, Result<Value, Error>> + Send + 'static,
) -> Result<Value, Error> {
    let parent = CURRENT.try_with(Clone::clone).ok();
    let plan = match parent.as_ref().map(|parent| &parent.plan) {
        Some(Plan::Frozen(plan)) => plan.clone(),
        _ => source(),
    };
    let call_id = uuid::Uuid::now_v7().to_string();
    let parent_call_id = parent.as_ref().map(|parent| parent.call_id.clone());
    let origin = Origin {
        request_id: parent
            .as_ref()
            .map_or_else(|| call_id.clone(), |parent| parent.request_id.clone()),
        cancellation: parent
            .as_ref()
            .map(|parent| parent.cancellation.clone())
            .unwrap_or_default(),
        extensions: parent.map(|parent| parent.extensions).unwrap_or_default(),
        call_id,
        plan: Plan::Frozen(plan.clone()),
    };
    let Some(plan) = plan.filter(FrozenMiddlewarePlan::has_service) else {
        return tokio::select! {
            biased;
            () = origin.cancellation.cancelled() => Err(Error::unavailable("服务调用已取消")),
            result = scope(origin.clone(), terminal(input)) => result,
        };
    };
    let context = Context {
        operation,
        request_id: origin.request_id,
        call_id: origin.call_id,
        parent_call_id,
        cancellation: origin.cancellation,
        extensions: origin.extensions,
        plan: plan.clone(),
    };
    let inner = context.clone();
    let next = compose(Vec::new(), move |input| {
        Box::pin(scope(inner, terminal(input)))
    });
    tokio::select! {
        biased;
        () = context.cancellation.cancelled() => Err(Error::unavailable("服务调用已取消")),
        result = plan.handle_service(context.clone(), input, next) => result,
    }
}

pub(crate) fn encode_error(error: AdminError) -> Error {
    Error {
        kind: match error.kind() {
            AdminErrorKind::Invalid => "invalid",
            AdminErrorKind::Unauthorized => "unauthorized",
            AdminErrorKind::Forbidden => "forbidden",
            AdminErrorKind::NotFound => "not_found",
            AdminErrorKind::Conflict => "conflict",
            AdminErrorKind::RateLimited => "rate_limited",
            AdminErrorKind::BadGateway => "bad_gateway",
            AdminErrorKind::UpstreamResultUnknown => "upstream_result_unknown",
            AdminErrorKind::Unavailable => "unavailable",
            AdminErrorKind::Internal => "internal",
        }
        .into(),
        message: error.message().to_owned(),
        details: None,
    }
}
