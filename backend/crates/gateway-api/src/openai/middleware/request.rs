//! 请求身份冻结、单次 next 与调用代次保活；HTTP 编码在 http 子模块中完成

use crate::middleware::headers::{decode_headers, encode_headers};

use crate::openai::error::gateway_error_response;
use axum::{http::HeaderMap, response::Response};
use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::{
    engine::{
        execution::{
            AuthenticatedClient, ClientTransport, ExecutionService, PreparedRootExecution,
        },
        middleware::{
            FrozenMiddlewarePlan, MiddlewareBody, MiddlewareError, MiddlewareFrame,
            MiddlewareHeader, MiddlewareMount, MiddlewareRequest, MiddlewareResponse,
            MiddlewareTarget,
        },
    },
    lifecycle::CancellationToken,
    operation::OperationKind,
};
use std::{future::Future, sync::Arc};

use super::http::{ExpectedBody, buffered_response, error_response, into_http_response};

pub(crate) struct RequestInput {
    pub endpoint: String,
    pub protocol: String,
    pub operation: Option<OperationKind>,
    pub transport: ClientTransport,
    pub model_hint: Option<String>,
    pub headers: Vec<MiddlewareHeader>,
    pub body: Bytes,
}

impl RequestInput {
    pub(crate) fn query(endpoint: String, model_hint: Option<String>, headers: &HeaderMap) -> Self {
        Self {
            endpoint,
            protocol: "openai".to_owned(),
            operation: None,
            transport: ClientTransport::HttpJson,
            model_hint,
            headers: encode_headers(headers),
            body: Bytes::new(),
        }
    }
}

/// 已鉴权只读查询共用入口；不重新鉴权、计量或创建 Provider attempt
pub(crate) async fn query_response<F, Fut>(
    execution: Arc<dyn ExecutionService>,
    client: AuthenticatedClient,
    input: RequestInput,
    terminal: F,
) -> Response
where
    F: FnOnce(AuthenticatedClient) -> Fut + Send + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    let prepared = match execution.prepare_verified_execution(client) {
        Ok(prepared) => prepared,
        Err(error) => return gateway_error_response(&error),
    };
    let protocol = input.protocol.clone();
    let result = invoke_request(
        execution,
        prepared,
        input,
        Box::new(move |prepared, request| {
            Box::pin(async move {
                if request.protocol() != protocol || !request.body().is_empty() {
                    return Err(MiddlewareError::Rejected);
                }
                buffered_response(&protocol, terminal(prepared.client().clone()).await).await
            })
        }),
    )
    .await;
    match result {
        Ok(response) => into_http_response(response, ExpectedBody::SingleJson).await,
        Err(error) => error_response(error),
    }
}

pub(crate) type RequestTerminal = Box<
    dyn FnOnce(
            PreparedRootExecution,
            MiddlewareRequest,
        ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>>
        + Send,
>;

pub(crate) async fn invoke_request(
    execution: Arc<dyn ExecutionService>,
    mut prepared: PreparedRootExecution,
    input: RequestInput,
    terminal: RequestTerminal,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let origin = crate::middleware::current();
    if let Some(origin) = &origin {
        prepared = prepared.with_extension_scope(origin.extensions.clone());
    }
    let plan = origin
        .and_then(|origin| origin.plan)
        .or_else(|| execution.middleware_plan(&prepared));
    let lifetime = RequestLifetime {
        cancellation: prepared.cancellation(),
        _plan: plan.clone(),
    };
    let context = prepared.middleware_context(
        MiddlewareTarget {
            request_id: prepared.request_id().clone(),
            mount: MiddlewareMount::Request,
            attempt_index: None,
            operation: input.operation,
            endpoint: input.endpoint,
            transport: input.transport,
            provider: None,
            model: input.model_hint,
            account_id: None,
        },
        prepared.extension_scope(),
    );
    let mut request = MiddlewareRequest::new(input.protocol, input.headers, input.body);
    if plan.is_some() {
        request = request.with_settings(prepared.request_settings());
    }
    let next = gateway_core::middleware::compose(Vec::new(), move |request: MiddlewareRequest| {
        Box::pin(async move {
            if let Some(settings) = request.settings() {
                prepared
                    .apply_settings(settings)
                    .map_err(MiddlewareError::Gateway)?;
            }
            terminal(prepared, request).await
        })
    });
    let response = match plan {
        Some(plan) => plan.handle(context, request, next).await?,
        None => next.run(request).await?,
    };
    let (protocol, status, headers, body, envelope) = response.into_parts();
    let mut response = MiddlewareResponse::new(
        protocol,
        status,
        headers,
        Box::new(RequestBody { body, lifetime }),
    );
    if let Some(envelope) = envelope {
        response = response.with_envelope(envelope);
    }
    Ok(response)
}

struct RequestLifetime {
    cancellation: CancellationToken,
    _plan: Option<FrozenMiddlewarePlan>,
}

impl Drop for RequestLifetime {
    fn drop(&mut self) {
        // 包括还在 next 中等待、客户端断开和提前关流；不启动第二份账本终结逻辑
        self.cancellation.cancel();
    }
}

struct RequestBody {
    body: Box<dyn MiddlewareBody>,
    lifetime: RequestLifetime,
}

impl MiddlewareBody for RequestBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        self.body.next_frame()
    }

    fn commit_downstream(
        &mut self,
        status: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        self.body.commit_downstream(status)
    }

    fn record_client_status(&mut self, status: u16) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        self.body.record_client_status(status)
    }

    fn is_finalized(&self) -> bool {
        self.body.is_finalized()
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let Self { body, lifetime } = *self;
            body.close().await;
            drop(lifetime);
        })
    }
}

/// 终点重新解码插件改写的请求；冻结认证身份不从这些 header 再次取得
pub(crate) fn request_parts(
    request: MiddlewareRequest,
) -> Result<(String, HeaderMap, Bytes), MiddlewareError> {
    let (protocol, headers, body) = request.into_parts();
    Ok((protocol, decode_headers(headers)?, body))
}
