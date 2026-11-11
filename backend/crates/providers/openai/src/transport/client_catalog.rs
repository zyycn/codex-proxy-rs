//! 上游模型目录读取；Codex 版本协商与 Messages 分页各自遵循原生合同

use gateway_protocol::openai::events::retry_after_seconds_from_body;

use super::{
    catalog::{CodexModelCatalogSnapshot, catalog_etag, parse_codex_model_catalog},
    client::{
        CodexBackendClient, CodexBackendTransport, CodexClientError, CodexClientResult,
        CodexRequestContext, OpenAiUpstreamProtocol, retry_after_seconds,
    },
    diagnostics::CodexUpstreamSendPhase,
    endpoints::endpoint_url,
    profile::CodexWireProfile,
    response_meta,
};

impl CodexBackendClient {
    /// 客户端目录按调用方版本协商；后台目录仍使用经过核验的服务端画像版本
    pub async fn fetch_models_with_context(
        &self,
        context: CodexRequestContext<'_>,
        client_version: Option<&str>,
    ) -> CodexClientResult<CodexModelCatalogSnapshot> {
        let profile = self.profile.snapshot();
        let query = match self.protocol {
            OpenAiUpstreamProtocol::Codex | OpenAiUpstreamProtocol::ResponsesApi => (
                "client_version",
                client_version.unwrap_or(&profile.codex_version),
            ),
            OpenAiUpstreamProtocol::MessagesApi => ("limit", "1000"),
        };
        let (body, etag) = match self.fetch_models_page(context, &profile, &[query]).await {
            Err(CodexClientError::Upstream {
                status, ref body, ..
            }) if self.protocol == OpenAiUpstreamProtocol::ResponsesApi
                && rejects_catalog_version(status, body) =>
            {
                self.fetch_models_page(context, &profile, &[]).await?
            }
            result => result?,
        };
        match self.protocol {
            OpenAiUpstreamProtocol::Codex => Ok(parse_codex_model_catalog(&body, etag.as_deref())?),
            OpenAiUpstreamProtocol::ResponsesApi => Ok(super::catalog::parse_api_model_catalog(
                &body,
                etag.as_deref(),
                self.protocol,
            )?),
            OpenAiUpstreamProtocol::MessagesApi => {
                let mut page = body;
                let mut models = Vec::new();
                let mut cursors = std::collections::BTreeSet::new();
                loop {
                    let mut value: serde_json::Value = serde_json::from_slice(&page)
                        .map_err(|_| super::CodexModelCatalogError::InvalidWire)?;
                    let data = value
                        .get_mut("data")
                        .and_then(serde_json::Value::as_array_mut)
                        .ok_or(super::CodexModelCatalogError::InvalidWire)?;
                    models.append(data);
                    if models.len() > super::catalog::MAX_CATALOG_MODELS {
                        return Err(super::CodexModelCatalogError::TooManyModels.into());
                    }
                    if value.get("has_more").and_then(serde_json::Value::as_bool) != Some(true) {
                        break;
                    }
                    let cursor = value
                        .get("last_id")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty())
                        .ok_or(super::CodexModelCatalogError::InvalidWire)?;
                    if !cursors.insert(cursor.to_owned()) {
                        return Err(super::CodexModelCatalogError::InvalidWire.into());
                    }
                    (page, _) = self
                        .fetch_models_page(context, &profile, &[query, ("after_id", cursor)])
                        .await?;
                }
                let body = serde_json::to_vec(&serde_json::json!({"data":models}))
                    .map_err(|_| super::CodexModelCatalogError::InvalidWire)?;
                Ok(super::catalog::parse_api_model_catalog(
                    &body,
                    etag.as_deref(),
                    self.protocol,
                )?)
            }
        }
    }

    async fn fetch_models_page(
        &self,
        context: CodexRequestContext<'_>,
        profile: &CodexWireProfile,
        query: &[(&str, &str)],
    ) -> CodexClientResult<(bytes::Bytes, Option<String>)> {
        let path = match self.protocol {
            OpenAiUpstreamProtocol::Codex => "codex/models",
            OpenAiUpstreamProtocol::ResponsesApi | OpenAiUpstreamProtocol::MessagesApi => "models",
        };
        let headers = self.model_request_headers(profile, context)?;
        let mut request = self
            .client
            .get(endpoint_url(&self.base_url, path))
            .headers(headers)
            .query(query)
            .build()?;
        if let Some(route) = self.route_to(request.url()) {
            request
                .headers_mut()
                .insert(gateway_protocol::peer::ROUTE_HEADER, route.clone());
        }
        let response = self.client.execute(request).await?;
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
        Ok((body, etag))
    }
}

/// 仅在上游明确不接受版本参数时读取普通目录，其他失败保留原错误
fn rejects_catalog_version(status: reqwest::StatusCode, body: &str) -> bool {
    if !matches!(status.as_u16(), 400 | 422) {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let error = &value["error"];
    error["param"].as_str() == Some("client_version")
        && matches!(
            error["code"].as_str(),
            Some("unknown_parameter" | "unsupported_parameter")
        )
}
