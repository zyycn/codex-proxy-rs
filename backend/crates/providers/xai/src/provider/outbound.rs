//! 原生编码完成后准备 xAI 请求，保留所选账号和出口绑定

use gateway_core::engine::middleware::outbound::{Request, Target};

use super::*;

/// 直连使用独立的出口池键，账号租约及后台刷新保留原代理绑定
pub(super) fn model_request_binding(
    session: &SelectedGrokSession,
    context: &AttemptContext,
) -> crate::transport::GrokSessionBinding {
    let binding = session.binding().clone();
    if context.uses_direct_fallback(session.account_id()) {
        context.trace().record(
            "outbound.direct_fallback",
            serde_json::json!({
                "accountId": session.account_id().as_str(),
                "proxyEndpoint": binding.outbound_proxy().map(|proxy| proxy.endpoint()),
            }),
        );
        binding.with_outbound_proxy(None)
    } else {
        binding
    }
}

/// 只为开启回退的模型请求使用共享 opening 预算，不限制模型响应等待
pub(super) async fn execute_model_request(
    transport: &dyn GrokInferenceTransport,
    request: GrokInferenceRequest,
    context: &AttemptContext,
    session: &SelectedGrokSession,
) -> Result<GrokInferenceResponse, Box<GrokInferenceTransportError>> {
    if !context.uses_direct_fallback(session.account_id())
        && !(context.proxy_direct_fallback_enabled()
            && session.binding().outbound_proxy().is_some())
    {
        return transport.execute(request).await.map_err(Box::new);
    }
    let budget = context.connection_budget();
    let opening_budget = if context.uses_direct_fallback(session.account_id()) {
        budget.clone()
    } else {
        budget.clone().reserve_next_opening()
    };
    let remaining = opening_budget.begin().map_err(|_| {
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::Timeout,
            UpstreamSendState::NotSent,
        )
    })?;
    let result = transport
        .execute(request.with_connect_timeout(remaining))
        .await;
    if result.is_ok()
        || result
            .as_ref()
            .is_err_and(|error| error.send_state() != UpstreamSendState::NotSent)
    {
        budget.complete();
    }
    result.map_err(Box::new)
}

pub(super) async fn prepare_request(
    request: GrokInferenceRequest,
    context: &AttemptContext,
    session: &SelectedGrokSession,
    upstream_model: &UpstreamModelId,
) -> Result<GrokInferenceRequest, ProviderError> {
    let Some(pipeline) = context.outbound_middleware(
        ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| invalid_request())?,
        Some(upstream_model.as_str().to_owned()),
        session.account_id().clone(),
    ) else {
        return Ok(request);
    };
    let prepared = pipeline
        .prepare(Request {
            target: Target::Http {
                method: request.method().clone(),
                uri: request
                    .endpoint()
                    .as_str()
                    .parse()
                    .map_err(|_| invalid_request())?,
            },
            headers: request.headers().to_vec(),
            body: bytes::Bytes::copy_from_slice(request.body()),
        })
        .await?;
    prepared.header_map()?;
    let Target::Http { method, uri } = prepared.target else {
        return Err(invalid_request());
    };
    let endpoint = Url::parse(&uri.to_string()).map_err(|_| invalid_request())?;
    Ok(request.with_prepared(method, endpoint, prepared.headers, prepared.body))
}

fn invalid_request() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidRequest,
        UpstreamSendState::NotSent,
    )
    .with_retry_prohibited()
}
