//! 原生请求完成编码后的插件准备，不持有发送与续接决策

use std::collections::BTreeSet;

use bytes::Bytes;
use gateway_core::{
    engine::middleware::{
        MiddlewareHeader,
        outbound::{Pipeline, Request, Target},
    },
    error::{ProviderError, ProviderErrorKind},
    upstream::UpstreamSendState,
};
use reqwest::{Method, Url, header::HeaderMap};
use sha2::{Digest, Sha256};
use tungstenite::http;

use super::{
    client::{CodexBackendClient, CodexClientError, CodexClientResult},
    websocket::CodexWebSocketRequest,
};

impl CodexBackendClient {
    pub(crate) fn with_outbound_middleware(mut self, pipeline: Option<Pipeline>) -> Self {
        self.outbound_middleware = pipeline;
        self
    }

    pub(super) async fn prepare_http_request(
        &self,
        endpoint: String,
        headers: HeaderMap,
        body: Bytes,
    ) -> CodexClientResult<reqwest::Request> {
        let Some(pipeline) = &self.outbound_middleware else {
            // 未命中出站插件时沿用原生 RequestBuilder 的请求构造语义
            return self
                .client
                .post(endpoint)
                .headers(headers)
                .body(body)
                .build()
                .map_err(CodexClientError::Http);
        };
        let prepared = pipeline
            .prepare(Request {
                target: Target::Http {
                    method: Method::POST,
                    uri: endpoint.parse().map_err(|_| invalid_request())?,
                },
                headers: headers
                    .iter()
                    .map(|(name, value)| {
                        MiddlewareHeader::new(
                            name.as_str(),
                            Bytes::copy_from_slice(value.as_bytes()),
                        )
                    })
                    .collect(),
                body,
            })
            .await
            .map_err(|error| CodexClientError::Outbound(Box::new(error)))?;
        let headers = prepared
            .header_map()
            .map_err(|error| CodexClientError::Outbound(Box::new(error)))?;
        let Target::Http { method, uri } = prepared.target else {
            return Err(invalid_request());
        };
        let url = Url::parse(&uri.to_string()).map_err(|_| invalid_request())?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(invalid_request());
        }
        let mut request = reqwest::Request::new(method, url);
        *request.headers_mut() = headers;
        *request.body_mut() = Some(prepared.body.into());
        Ok(request)
    }

    pub(super) async fn prepare_websocket_request(
        &self,
        mut request: CodexWebSocketRequest,
    ) -> CodexClientResult<(CodexWebSocketRequest, Option<String>)> {
        let Some(pipeline) = &self.outbound_middleware else {
            return Ok((request, None));
        };
        let uri: http::Uri = request
            .connection()
            .endpoint()
            .parse()
            .map_err(|_| invalid_request())?;
        let outbound = Request {
            target: Target::WebSocket { uri: uri.clone() },
            headers: request
                .connection()
                .headers()
                .iter()
                .map(|(name, value)| {
                    MiddlewareHeader::new(name.clone(), Bytes::copy_from_slice(value.as_bytes()))
                })
                .collect(),
            body: request.take_payload_text().into(),
        };
        let original_headers = outbound
            .header_map()
            .map_err(|error| CodexClientError::Outbound(Box::new(error)))?;
        let prepared = pipeline
            .prepare(outbound)
            .await
            .map_err(|error| CodexClientError::Outbound(Box::new(error)))?;
        let headers = prepared
            .header_map()
            .map_err(|error| CodexClientError::Outbound(Box::new(error)))?;
        let Target::WebSocket { uri: prepared_uri } = prepared.target else {
            return Err(invalid_request());
        };
        let endpoint = prepared_uri.to_string();
        let url = Url::parse(&endpoint).map_err(|_| invalid_request())?;
        if !matches!(url.scheme(), "ws" | "wss") || url.host_str().is_none() {
            return Err(invalid_request());
        }
        let profile = opening_changes(&uri, &prepared_uri, &original_headers, &headers);
        let headers = prepared
            .headers
            .into_iter()
            .map(|header| {
                let (name, value) = header.into_parts();
                Ok((
                    name,
                    std::str::from_utf8(&value)
                        .map_err(|_| invalid_request())?
                        .to_owned(),
                ))
            })
            .collect::<CodexClientResult<Vec<_>>>()?;
        let payload = String::from_utf8(prepared.body.into()).map_err(|_| invalid_request())?;
        // 只替换候选 opening 和正文；原请求的 continuation 归属不从插件正文重建
        request.replace_outbound(endpoint, headers, payload);
        request
            .connection
            .validate_prepared_opening()
            .map_err(|_| invalid_request())?;
        Ok((request, profile))
    }
}

fn invalid_request() -> CodexClientError {
    CodexClientError::Outbound(Box::new(
        ProviderError::new(
            ProviderErrorKind::InvalidRequest,
            UpstreamSendState::NotSent,
        )
        .with_retry_prohibited(),
    ))
}

fn opening_changes(
    original_uri: &http::Uri,
    uri: &http::Uri,
    original: &HeaderMap,
    headers: &HeaderMap,
) -> Option<String> {
    let mut digest = Sha256::new();
    let mut changed = original_uri != uri;
    if changed {
        digest.update(uri.to_string().as_bytes());
    }
    let names: BTreeSet<_> = original
        .keys()
        .chain(headers.keys())
        .map(|name| name.as_str())
        .collect();
    for name in names {
        if original
            .get_all(name)
            .iter()
            .eq(headers.get_all(name).iter())
        {
            continue;
        }
        changed = true;
        digest.update(name.len().to_le_bytes());
        digest.update(name.as_bytes());
        digest.update(headers.get_all(name).iter().count().to_le_bytes());
        for value in headers.get_all(name) {
            digest.update(value.len().to_le_bytes());
            digest.update(value.as_bytes());
        }
    }
    // 原生随机握手键和追踪头未被插件改写时，不影响新链复用
    changed.then(|| hex::encode(digest.finalize()))
}
