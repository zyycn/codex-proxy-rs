//! Codex 与 Responses API 模型目录的 HTTP 读取与客户端版本协商

use gateway_protocol::openai::events::retry_after_seconds_from_body;

use super::{
    catalog::{CodexModelCatalogSnapshot, catalog_etag, parse_codex_model_catalog},
    client::{
        CodexBackendClient, CodexBackendTransport, CodexClientError, CodexClientResult,
        CodexRequestContext, OpenAiUpstreamProtocol, retry_after_seconds,
    },
    diagnostics::CodexUpstreamSendPhase,
    endpoints::endpoint_url,
    response_meta,
};

impl CodexBackendClient {
    /// 客户端目录按调用方版本协商；后台目录仍使用经过核验的服务端画像版本
    pub async fn fetch_models_with_context(
        &self,
        context: CodexRequestContext<'_>,
        client_version: Option<&str>,
    ) -> CodexClientResult<CodexModelCatalogSnapshot> {
        let path = match self.protocol {
            OpenAiUpstreamProtocol::Codex => "codex/models",
            OpenAiUpstreamProtocol::ResponsesApi => "models",
        };
        let profile = self.profile.snapshot();
        let headers = self.model_request_headers(&profile, context)?;
        let mut request = self
            .client
            .get(endpoint_url(&self.base_url, path))
            .headers(headers);
        let client_version = client_version.or_else(|| {
            (self.protocol == OpenAiUpstreamProtocol::Codex)
                .then_some(profile.codex_version.as_str())
        });
        if let Some(client_version) = client_version {
            request = request.query(&[("client_version", client_version)]);
        }
        let response = request.send().await?;
        let status = response.status();
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers())
            .with_response_origin(Some(response.url().clone()));
        let set_cookie_headers = response_meta::set_cookie_headers(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);
        let etag = status
            .is_success()
            .then(|| catalog_etag(response.headers()))
            .transpose()?
            .flatten();
        let body = response.bytes().await?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&body).into_owned();
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
                client_response: None,
                diagnostics: Box::new(diagnostics),
                set_cookie_headers,
                rate_limit_headers: Vec::new(),
                transport: CodexBackendTransport::HttpSse,
                transport_metrics: Box::default(),
                send_phase: CodexUpstreamSendPhase::AfterPayload,
            });
        }
        Ok(match self.protocol {
            OpenAiUpstreamProtocol::Codex => parse_codex_model_catalog(&body, etag.as_deref())?,
            OpenAiUpstreamProtocol::ResponsesApi => {
                super::catalog::parse_api_model_catalog(&body, etag.as_deref())?
            }
        })
    }
}
