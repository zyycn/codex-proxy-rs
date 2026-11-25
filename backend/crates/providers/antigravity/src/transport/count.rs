//! 原生计数响应保留独立 wire，不产生生成用量或费用

use super::{ResponseContext, sent};
use futures::Stream;
use gateway_core::{
    error::{ProviderError, ProviderErrorKind as Kind},
    event::{
        FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseObservation,
        ProviderResponseTimings, ResponseMeta,
    },
    upstream::UpstreamTransport,
};
use gateway_protocol::{genai::GENAI_PROTOCOL, openai::sse::MAX_SSE_EVENT_BUFFER_BYTES};
use serde_json::Value;

pub(crate) fn count_events(
    mut response: reqwest::Response,
    context: ResponseContext,
) -> impl Stream<Item = Result<ProviderEvent, ProviderError>> + Send + 'static {
    let headers_ms = u64::try_from(context.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    async_stream::try_stream! {
        let status = response.status().as_u16();
        let mut body = Vec::new();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = context.cancel.cancelled() => Err(sent(Kind::Cancelled)),
                _ = context.deadline.wait() => Err(sent(Kind::Timeout)),
                result = response.chunk() => result.map_err(|e| sent(Kind::Transport).with_source(e.without_url())),
            }?;
            let Some(chunk) = chunk else { break; };
            if body.len().saturating_add(chunk.len()) > MAX_SSE_EVENT_BUFFER_BYTES {
                Err(sent(Kind::MessageTooBig))?;
            }
            body.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&body).map_err(|_| sent(Kind::Protocol))?;
        // Cloud Code 的 protobuf JSON 会省略零值；公开计数接口显式交付 totalTokens
        if value.as_object().is_some_and(serde_json::Map::is_empty) {
            body = br#"{"totalTokens":0}"#.to_vec();
        } else if value.get("totalTokens").and_then(Value::as_u64).is_none() {
            Err(sent(Kind::Protocol))?;
        }
        let observation = ProviderResponseObservation::new(
            UpstreamTransport::new("http").map_err(|_| sent(Kind::ProviderInfrastructureUnavailable))?,
        ).with_status_code(status).with_timings(ProviderResponseTimings {
            headers_ms: Some(headers_ms), ..Default::default()
        });
        let meta = ResponseMeta::new(uuid::Uuid::now_v7().to_string(), context.model.as_str());
        let wire = ProtocolWireEvent::raw_json(GENAI_PROTOCOL, body.into()).map_err(|_| sent(Kind::Protocol))?;
        let mut event = ProviderEvent::wire(wire)
            .with_fact(GatewayEvent::Started(meta.clone()))
            .with_fact(GatewayEvent::Completed(meta.with_finish_reason(FinishReason::Stop)));
        event.attach_observation(observation);
        yield event;
    }
}
