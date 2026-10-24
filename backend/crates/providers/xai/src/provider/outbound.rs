//! 原生编码完成后准备 xAI 请求，保留所选账号和出口绑定

use gateway_core::engine::middleware::outbound::{Request, Target};

use super::*;

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
