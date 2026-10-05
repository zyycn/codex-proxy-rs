//! 通用公开服务回调；操作目录与类型校验由业务服务登记

use super::{CallbackScope, MiddlewareCallback, invalid};
use crate::RpcReply;
use futures::future::BoxFuture;
use gateway_admin::{model::AdminError, service::Registry};
use gateway_core::middleware::service as core;
use gateway_plugin_sdk::{
    CallContext, PluginFault,
    call::{middleware::NEXT_METHOD, services as wire},
};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Default)]
pub(crate) struct ServicePorts {
    registry: OnceLock<Weak<Registry>>,
    http: OnceLock<Weak<dyn gateway_core::middleware::http::Dispatcher>>,
}

impl ServicePorts {
    pub(crate) fn bind_http(
        &self,
        dispatcher: &Arc<dyn gateway_core::middleware::http::Dispatcher>,
    ) -> Result<(), AdminError> {
        self.http
            .set(Arc::downgrade(dispatcher))
            .map_err(|_| AdminError::conflict("HTTP 分派端口已经绑定"))
    }

    pub(super) fn http(
        &self,
    ) -> Result<Arc<dyn gateway_core::middleware::http::Dispatcher>, PluginFault> {
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
        scope: &CallbackScope,
        cancellation: gateway_core::lifecycle::CancellationToken,
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
        let origin = scope.origin.as_ref();
        let parent = gateway_admin::service::Origin {
            request_id: origin.map_or_else(
                || {
                    context
                        .request_id
                        .clone()
                        .unwrap_or_else(|| context.resource_scope_id.clone())
                },
                |origin| origin.request_id.clone(),
            ),
            call_id: origin.map_or_else(
                || format!("{}:{}", context.resource_scope_id, context.call_id),
                |origin| origin.call_id.clone(),
            ),
            cancellation,
            extensions: scope.child_extensions(&context.instance_id)?,
            plan: origin.map_or(gateway_admin::service::Plan::Current, |origin| {
                gateway_admin::service::Plan::Frozen(Some(origin.plan.clone()))
            }),
        };
        let result = gateway_admin::service::scope(parent, call).await;
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
