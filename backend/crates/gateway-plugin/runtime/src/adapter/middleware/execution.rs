//! 选择挂载边界并用 Core 续体组合中间件链

use super::{MiddlewareEntry, PluginMiddlewarePlan};
use futures::future::BoxFuture;
use gateway_core::engine::middleware::{
    MiddlewareContext, MiddlewareError, MiddlewareMount, MiddlewareNext, MiddlewarePlan,
    MiddlewareRequest, MiddlewareResponse,
};
use gateway_core::middleware::{Middleware, Next};
use std::{sync::Arc, time::Duration};

// 协议适配只提供选择条件与单层调用；顺序、上下文捕获和续体拼接共用这一个入口
type Invoke<Context, Input, Output, Error> = fn(
    Arc<MiddlewareEntry>,
    Context,
    Input,
    Next<Input, Output, Error>,
    Duration,
) -> BoxFuture<'static, Result<Output, Error>>;

impl PluginMiddlewarePlan {
    fn chain<Context, Input, Output, Error>(
        &self,
        mount: MiddlewareMount,
        context: Context,
        select: impl Fn(&MiddlewareEntry, &Context) -> bool,
        invoke: Invoke<Context, Input, Output, Error>,
        next: Next<Input, Output, Error>,
    ) -> Next<Input, Output, Error>
    where
        Context: Clone + Send + 'static,
        Input: Send + 'static,
        Output: Send + 'static,
        Error: Send + 'static,
    {
        let Some(entries) = self.middleware.get(&mount) else {
            return next;
        };
        let layers = entries
            .iter()
            .filter(|entry| select(entry, &context))
            .map(|entry| -> Middleware<Input, Output, Error> {
                let entry = Arc::clone(entry);
                let context = context.clone();
                let timeout = self.middleware_timeout;
                Box::new(move |input, next| invoke(entry, context, input, next, timeout))
            })
            .collect();
        gateway_core::middleware::compose(layers, move |input| next.run(input))
    }
}

impl MiddlewarePlan for PluginMiddlewarePlan {
    fn has_outbound(&self, context: &MiddlewareContext) -> bool {
        self.middleware
            .get(&MiddlewareMount::Outbound)
            .is_some_and(|entries| entries.iter().any(|entry| outbound_matches(entry, context)))
    }

    fn handle_outbound(
        &self,
        context: MiddlewareContext,
        request: gateway_core::engine::middleware::outbound::Request,
        next: gateway_core::engine::middleware::outbound::Next,
    ) -> BoxFuture<
        'static,
        Result<gateway_core::engine::middleware::outbound::Request, MiddlewareError>,
    > {
        self.chain(
            MiddlewareMount::Outbound,
            context,
            outbound_matches,
            |entry, context, request, next, timeout| {
                Box::pin(super::outbound::invoke(
                    entry, context, request, next, timeout,
                ))
            },
            next,
        )
        .run(request)
    }

    fn has_service(&self) -> bool {
        self.middleware.contains_key(&MiddlewareMount::Service)
    }
    fn handle_service(
        &self,
        context: gateway_core::engine::middleware::service::Context,
        input: gateway_core::engine::middleware::service::Value,
        next: gateway_core::engine::middleware::service::Next,
    ) -> BoxFuture<
        'static,
        Result<
            gateway_core::engine::middleware::service::Value,
            gateway_core::engine::middleware::service::Error,
        >,
    > {
        self.chain(
            MiddlewareMount::Service,
            context,
            |entry, context| !context.extensions.contains(&entry.instance_id),
            |entry, context, input, next, timeout| {
                Box::pin(super::service::invoke(entry, context, input, next, timeout))
            },
            next,
        )
        .run(input)
    }

    fn has_websocket(&self) -> bool {
        self.middleware.contains_key(&MiddlewareMount::WebSocket)
    }
    fn handle_websocket(
        &self,
        context: gateway_core::engine::middleware::websocket::Context,
        message: gateway_core::engine::middleware::websocket::Message,
        next: gateway_core::engine::middleware::websocket::Next,
    ) -> BoxFuture<
        'static,
        Result<Option<gateway_core::engine::middleware::websocket::Message>, MiddlewareError>,
    > {
        self.chain(
            MiddlewareMount::WebSocket,
            context,
            |_entry, _context| true,
            |entry, context, message, next, timeout| {
                Box::pin(super::websocket::invoke(
                    entry, context, message, next, timeout,
                ))
            },
            next,
        )
        .run(message)
    }
    fn has_http(&self) -> bool {
        self.middleware.contains_key(&MiddlewareMount::Http)
    }

    fn handle_http(
        &self,
        context: gateway_core::engine::middleware::http::Context,
        request: gateway_core::engine::middleware::http::Request,
        next: gateway_core::engine::middleware::http::Next,
    ) -> BoxFuture<'static, Result<gateway_core::engine::middleware::http::Response, MiddlewareError>>
    {
        self.chain(
            MiddlewareMount::Http,
            context,
            |entry, context| !context.extensions.contains(&entry.instance_id),
            |entry, context, request, next, timeout| {
                Box::pin(super::http::invoke(entry, context, request, next, timeout))
            },
            next,
        )
        .run(request)
    }

    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        self.chain(
            context.mount(),
            context,
            |entry, context| {
                !context.extension_scope().contains(&entry.instance_id)
                    && entry.scope.matches_processing(
                        context.client_key_id(),
                        context.account_group_ids(),
                        context.provider(),
                        context.model(),
                    )
            },
            |entry, context, request, next, timeout| {
                Box::pin(super::request::invoke(
                    entry, context, request, next, timeout,
                ))
            },
            next,
        )
        .run(request)
    }
}

// 存在性检查与实际组合使用同一选择规则，未命中时不进入原生报文准备
fn outbound_matches(entry: &MiddlewareEntry, context: &MiddlewareContext) -> bool {
    !context.extension_scope().contains(&entry.instance_id)
        && entry.scope.matches_processing(
            context.client_key_id(),
            context.account_group_ids(),
            context.provider(),
            context.model(),
        )
}
