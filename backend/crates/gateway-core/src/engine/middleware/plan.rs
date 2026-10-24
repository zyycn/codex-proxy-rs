//! 与发布代次一起冻结的协议及请求中间件计划

use super::{
    MiddlewareContext, MiddlewareError, MiddlewareNext, MiddlewareRequest, MiddlewareResponse,
};
use crate::routing::extensions::ExtensionSetReference;
use futures::future::BoxFuture;
use std::{fmt, sync::Arc};

/// 一个发布代次冻结的统一数据面中间件计划
pub trait MiddlewarePlan: Send + Sync + fmt::Debug {
    fn has_outbound(&self, _context: &MiddlewareContext) -> bool {
        false
    }
    fn handle_outbound(
        &self,
        _context: MiddlewareContext,
        request: super::outbound::Request,
        next: super::outbound::Next,
    ) -> BoxFuture<'static, Result<super::outbound::Request, MiddlewareError>> {
        next.run(request)
    }

    fn has_service(&self) -> bool {
        false
    }
    fn handle_service(
        &self,
        _context: crate::engine::middleware::service::Context,
        input: crate::engine::middleware::service::Value,
        next: crate::engine::middleware::service::Next,
    ) -> BoxFuture<
        'static,
        Result<
            crate::engine::middleware::service::Value,
            crate::engine::middleware::service::Error,
        >,
    > {
        next.run(input)
    }

    fn has_websocket(&self) -> bool {
        false
    }
    fn handle_websocket(
        &self,
        _context: crate::engine::middleware::websocket::Context,
        message: crate::engine::middleware::websocket::Message,
        next: crate::engine::middleware::websocket::Next,
    ) -> BoxFuture<
        'static,
        Result<Option<crate::engine::middleware::websocket::Message>, MiddlewareError>,
    > {
        next.run(message)
    }

    /// HTTP 入口在认证、路由与正文解析之前组合，无匹配时保留直接路径
    fn has_http(&self) -> bool {
        false
    }

    fn handle_http(
        &self,
        _context: crate::engine::middleware::http::Context,
        request: crate::engine::middleware::http::Request,
        next: crate::engine::middleware::http::Next,
    ) -> BoxFuture<'static, Result<crate::engine::middleware::http::Response, MiddlewareError>>
    {
        next.run(request)
    }

    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>>;
}

/// 调用期间同时保活发布代次与中间件计划
#[derive(Clone)]
pub struct FrozenMiddlewarePlan {
    plan: Arc<dyn MiddlewarePlan>,
    _generation: ExtensionSetReference,
}

impl FrozenMiddlewarePlan {
    #[must_use]
    pub fn has_outbound(&self, context: &MiddlewareContext) -> bool {
        self.plan.has_outbound(context)
    }
    pub fn handle_outbound(
        &self,
        mut context: MiddlewareContext,
        request: super::outbound::Request,
        next: super::outbound::Next,
    ) -> BoxFuture<'static, Result<super::outbound::Request, MiddlewareError>> {
        context.plan = Some(self.clone());
        self.plan.handle_outbound(context, request, next)
    }

    #[must_use]
    pub fn has_service(&self) -> bool {
        self.plan.has_service()
    }
    pub fn handle_service(
        &self,
        context: crate::engine::middleware::service::Context,
        input: crate::engine::middleware::service::Value,
        next: crate::engine::middleware::service::Next,
    ) -> BoxFuture<
        'static,
        Result<
            crate::engine::middleware::service::Value,
            crate::engine::middleware::service::Error,
        >,
    > {
        self.plan.handle_service(context, input, next)
    }

    #[must_use]
    pub fn has_websocket(&self) -> bool {
        self.plan.has_websocket()
    }
    pub fn handle_websocket(
        &self,
        context: crate::engine::middleware::websocket::Context,
        message: crate::engine::middleware::websocket::Message,
        next: crate::engine::middleware::websocket::Next,
    ) -> BoxFuture<
        'static,
        Result<Option<crate::engine::middleware::websocket::Message>, MiddlewareError>,
    > {
        self.plan.handle_websocket(context, message, next)
    }

    #[must_use]
    pub fn has_http(&self) -> bool {
        self.plan.has_http()
    }

    pub fn handle_http(
        &self,
        mut context: crate::engine::middleware::http::Context,
        request: crate::engine::middleware::http::Request,
        next: crate::engine::middleware::http::Next,
    ) -> BoxFuture<'static, Result<crate::engine::middleware::http::Response, MiddlewareError>>
    {
        context.plan = Some(self.clone());
        self.plan.handle_http(context, request, next)
    }

    #[must_use]
    pub fn new(plan: Arc<dyn MiddlewarePlan>, generation: ExtensionSetReference) -> Self {
        Self {
            plan,
            _generation: generation,
        }
    }

    pub fn handle(
        &self,
        mut context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        context.plan = Some(self.clone());
        self.plan.handle(context, request, next)
    }
}

impl fmt::Debug for FrozenMiddlewarePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrozenMiddlewarePlan")
            .finish_non_exhaustive()
    }
}
