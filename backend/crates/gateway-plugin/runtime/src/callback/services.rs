//! 通用公开服务回调；操作目录与类型校验由业务服务登记

use super::{CallResources, MiddlewareCallback, invalid};
use crate::RpcReply;
use futures::future::BoxFuture;
use gateway_admin::{model::AdminError, public_service::Registry};
use gateway_core::engine::middleware::service as core;
use gateway_plugin_sdk::{
    CallContext, PluginFault,
    call::{middleware::NEXT_METHOD, services as wire},
};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Default)]
pub(crate) struct ServicePorts {
    registry: OnceLock<Weak<Registry>>,
    http: OnceLock<Weak<dyn gateway_core::engine::middleware::http::Dispatcher>>,
}

impl ServicePorts {
    pub(crate) fn bind_http(
        &self,
        dispatcher: &Arc<dyn gateway_core::engine::middleware::http::Dispatcher>,
    ) -> Result<(), AdminError> {
        self.http
            .set(Arc::downgrade(dispatcher))
            .map_err(|_| AdminError::conflict("HTTP 分派端口已经绑定"))
    }

    pub(super) fn http(
        &self,
    ) -> Result<Arc<dyn gateway_core::engine::middleware::http::Dispatcher>, PluginFault> {
        self.http.get().and_then(Weak::upgrade).ok_or_else(|| {
            PluginFault::new(
                gateway_plugin_sdk::ErrorCode::Fault,
                "HTTP dispatcher is unavailable",
            )
        })
    }

    pub(crate) fn bind(&self, registry: &Arc<Registry>) -> Result<(), AdminError> {
        self.registry
            .set(Arc::downgrade(registry))
            .map_err(|_| AdminError::conflict("公开服务已经绑定"))
    }

    pub(super) async fn call(
        &self,
        context: &CallContext,
        resources: &CallResources,
        actor: gateway_admin::model::usage_data::UsageActor,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        if !payload.is_empty() {
            return Err(invalid());
        }
        let request: wire::Request = serde_json::from_value(params).map_err(|_| invalid())?;
        let registry = self.registry.get().and_then(Weak::upgrade).ok_or_else(|| {
            gateway_plugin_sdk::PluginFault::new(
                gateway_plugin_sdk::ErrorCode::Fault,
                "公开服务尚未绑定",
            )
        })?;
        let call = registry.call(&request.operation, request.input);
        resources.scope.check_child_depth(&context.instance_id)?;
        let origin = resources.child_origin(context)?;
        let parent = gateway_admin::public_service::Origin {
            request_id: origin.request_id,
            call_id: origin.call_id,
            cancellation: origin.cancellation,
            extensions: origin.extensions,
            plan: origin
                .plan
                .map_or(gateway_admin::public_service::Plan::Current, |plan| {
                    gateway_admin::public_service::Plan::Frozen(Some(plan))
                }),
        };
        // 即使终端只读，服务中间件也可提交写入；不存在的操作不会进入调用链
        if registry
            .operations()
            .any(|operation| operation == request.operation)
        {
            resources.observe_dispatch()?;
        }
        let result = gateway_admin::public_service::scope_actor(
            actor,
            gateway_admin::public_service::scope(parent, call),
        )
        .await;
        encode(result)
    }
}

pub(crate) struct Invocation(Mutex<Option<core::Next>>);

impl Invocation {
    pub(crate) fn new(next: core::Next) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Some(next))))
    }
}

impl MiddlewareCallback for Invocation {
    fn invoke(
        self: Arc<Self>,
        method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
        _maximum: usize,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        Box::pin(async move {
            if method != NEXT_METHOD || !payload.is_empty() {
                return Err(invalid());
            }
            let next = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .ok_or_else(invalid)?;
            encode(next.run(params).await)
        })
    }
}

pub(crate) fn encode(result: Result<core::Value, core::Error>) -> Result<RpcReply, PluginFault> {
    let result: wire::Response = result.map_err(|error| wire::ServiceError {
        kind: error.kind,
        message: error.message,
        details: error.details,
    });
    Ok(RpcReply {
        result: serde_json::to_value(result).map_err(|_| invalid())?,
        payload: Vec::new(),
    })
}
