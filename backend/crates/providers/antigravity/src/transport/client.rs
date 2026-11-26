//! 已注册资源服务的项目发现、模型查询和原生 HTTP 请求

use bytes::Bytes;
use gateway_core::{
    account::OutboundProxy,
    error::{ProviderError, ProviderErrorKind as Kind},
};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{json, value::RawValue};
use std::{collections::BTreeMap, time::Duration};
use url::Url;

use super::failure::{network, sent, successful, unsent};
use crate::profile::ClientProfile;
use gateway_core::engine::middleware::MiddlewareHeader;
// Desktop 独立搜索使用固定任务指令，常规生成与计数使用账号画像
static SEARCH_INSTRUCTION: std::sync::LazyLock<&RawValue> = std::sync::LazyLock::new(|| {
    serde_json::from_str(r#"{"parts":[{"text":"Search the web for the query and return a concise answer grounded in the sources."}]}"#).expect("valid static search instruction")
});

const GENERATION_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_CONTROL_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub(crate) struct Catalog {
    pub(crate) project: String,
    #[serde(rename = "webSearchModelIds", default)]
    pub(crate) web_search_models: Vec<String>,
    pub(crate) models: BTreeMap<String, Model>,
    #[serde(rename = "groupedModels", default)]
    pub(crate) grouped_models: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Model {
    #[serde(rename = "displayName", default)]
    pub(crate) name: String,
    #[serde(rename = "quotaInfo", default)]
    pub(crate) quota: Option<super::account::ModelQuota>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(rename = "tagDescription", default)]
    pub(crate) tag_description: Option<String>,
    #[serde(rename = "maxTokens", default)]
    pub(crate) context_window: Option<u64>,
    #[serde(rename = "maxOutputTokens", default)]
    pub(crate) max_output_tokens: Option<u64>,
    #[serde(rename = "supportsImages", default)]
    pub(crate) supports_images: Option<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenerateMode {
    Json,
    Stream,
    WebSearch,
}

impl From<bool> for GenerateMode {
    fn from(stream: bool) -> Self {
        if stream { Self::Stream } else { Self::Json }
    }
}

pub(crate) struct CloudCodeClient {
    client: Client,
    origin: Url,
    system_instruction: Box<RawValue>,
    use_quota_credits: bool,
}

impl CloudCodeClient {
    pub(crate) fn new(
        origin: &Url,
        proxy: Option<&OutboundProxy>,
        profile: &ClientProfile,
    ) -> Result<Self, ProviderError> {
        Self::with_client(origin, Self::build_client(proxy, profile)?, profile, false)
    }

    pub(super) fn build_client(
        proxy: Option<&OutboundProxy>,
        profile: &ClientProfile,
    ) -> Result<Client, ProviderError> {
        gateway_egress::http_client(proxy)
            .map_err(|e| unsent(Kind::ProviderInfrastructureUnavailable).with_source(e))?
            // 官方 AGY 使用 HTTP/1.1；显式选择 TLS 后端，避免工作区 feature 合并切到系统 TLS
            .use_rustls_tls()
            .http1_only()
            .http1_title_case_headers()
            .pool_max_idle_per_host(100)
            .read_timeout(GENERATION_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .user_agent(profile.user_agent())
            .build()
            .map_err(|e| {
                unsent(Kind::ProviderInfrastructureUnavailable).with_source(e.without_url())
            })
    }

    pub(super) fn with_client(
        origin: &Url,
        client: Client,
        profile: &ClientProfile,
        use_quota_credits: bool,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            client,
            origin: origin.clone(),
            use_quota_credits,
            system_instruction: RawValue::from_string(
                json!({"parts":[{"text":profile.system_prompt}]}).to_string(),
            )
            .map_err(|e| unsent(Kind::ProviderInfrastructureUnavailable).with_source(e))?,
        })
    }

    pub(super) fn post(&self, action: &str, token: &str) -> reqwest::RequestBuilder {
        let mut url = self.origin.clone();
        url.set_path(&format!("/v1internal:{action}"));
        self.client
            .post(url)
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT_ENCODING, "gzip")
    }

    pub(crate) async fn catalog(&self, token: &str) -> Result<Catalog, ProviderError> {
        let project = self.account_info(token).await?.project;
        self.catalog_for_project(token, project).await
    }

    pub(crate) async fn catalog_for_project(
        &self,
        token: &str,
        project: String,
    ) -> Result<Catalog, ProviderError> {
        #[derive(Deserialize)]
        struct Models {
            #[serde(rename = "webSearchModelIds", default)]
            web_search_models: Vec<String>,
            models: BTreeMap<String, Model>,
            #[serde(rename = "groupedModels", default)]
            grouped_models: serde_json::Value,
        }
        let models: Models = control(
            self.post("fetchAvailableModels", token)
                .timeout(Duration::from_secs(20))
                .json(&json!({"project":project})),
            token,
        )
        .await?;
        if models
            .models
            .keys()
            .any(|id| gateway_core::routing::UpstreamModelId::new(id).is_err())
        {
            return Err(sent(Kind::Protocol));
        }
        Ok(Catalog {
            project,
            models: models.models,
            grouped_models: models.grouped_models,
            web_search_models: models.web_search_models,
        })
    }

    pub(crate) async fn generate(
        &self,
        project: &str,
        model: &str,
        token: &str,
        body: &[u8],
        mode: GenerateMode,
        headers: &[MiddlewareHeader],
    ) -> Result<Response, ProviderError> {
        #[derive(Serialize)]
        struct Envelope<'a> {
            project: &'a str,
            model: &'a str,
            #[serde(rename = "userAgent")]
            user_agent: &'static str,
            #[serde(rename = "requestType")]
            request_type: &'static str,
            #[serde(rename = "enabledCreditTypes", skip_serializing_if = "Option::is_none")]
            enabled_credit_types: Option<[&'static str; 1]>,
            request: &'a RawValue,
        }
        let instruction = if mode == GenerateMode::WebSearch {
            *SEARCH_INSTRUCTION
        } else {
            &self.system_instruction
        };
        let request = super::request::request_body(body, instruction, false)?;
        let (send, media) = if mode == GenerateMode::Stream {
            (
                self.post("streamGenerateContent", token)
                    .query(&[("alt", "sse")]),
                "text/event-stream",
            )
        } else {
            (
                self.post("generateContent", token)
                    .timeout(GENERATION_TIMEOUT),
                "application/json",
            )
        };
        self.send(
            send.json(&Envelope {
                project,
                model,
                user_agent: "antigravity",
                request_type: if mode == GenerateMode::WebSearch {
                    "web_search"
                } else {
                    "agent"
                },
                enabled_credit_types: self
                    .use_quota_credits
                    .then_some([super::account::GOOGLE_ONE_AI]),
                request: &request,
            }),
            token,
            headers,
            media,
        )
        .await
    }

    pub(crate) async fn count_tokens(
        &self,
        token: &str,
        body: &[u8],
        headers: &[MiddlewareHeader],
    ) -> Result<Response, ProviderError> {
        // Desktop 的计数封套只接受 Vertex CountTokensRequest，没有项目和生成控制字段
        let request = super::request::request_body(body, &self.system_instruction, true)?;
        #[derive(Serialize)]
        struct Envelope<'a> {
            request: &'a RawValue,
        }
        self.send(
            self.post("countTokens", token)
                .timeout(GENERATION_TIMEOUT)
                .json(&Envelope { request: &request }),
            token,
            headers,
            "application/json",
        )
        .await
    }

    async fn send(
        &self,
        mut send: reqwest::RequestBuilder,
        token: &str,
        headers: &[MiddlewareHeader],
        media: &str,
    ) -> Result<Response, ProviderError> {
        for header in headers {
            if super::headers::managed_header(header.name()) {
                continue;
            }
            let name = reqwest::header::HeaderName::from_bytes(header.name().as_bytes())
                .map_err(|e| unsent(Kind::InvalidRequest).with_source(e))?;
            let value = reqwest::header::HeaderValue::from_bytes(header.value())
                .map_err(|e| unsent(Kind::InvalidRequest).with_source(e))?;
            send = send.header(name, value);
        }
        let response = send.send().await.map_err(network)?;
        let response = successful(super::response::decode(response), token).await?;
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case(media))
        {
            return Err(sent(Kind::Protocol));
        }
        Ok(response)
    }
}

pub(super) async fn control<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
    token: &str,
) -> Result<T, ProviderError> {
    let mut response = successful(
        super::response::decode(request.send().await.map_err(network)?),
        token,
    )
    .await?;
    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| sent(Kind::Transport).with_source(e.without_url()))?
    {
        if data.len().saturating_add(chunk.len()) > MAX_CONTROL_RESPONSE_BYTES {
            return Err(sent(Kind::MessageTooBig));
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).map_err(|e| sent(Kind::Protocol).with_source(e))
}

#[derive(Deserialize)]
struct Reply<'a> {
    #[serde(borrow)]
    response: Option<&'a RawValue>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
}

pub(crate) fn unwrap_frame(data: &str) -> Result<Bytes, ProviderError> {
    let envelope: Reply =
        serde_json::from_str(data).map_err(|e| sent(Kind::Protocol).with_source(e))?;
    match (envelope.response, envelope.error) {
        (None, Some(err)) if err.get().starts_with('{') => {
            Ok(Bytes::copy_from_slice(data.as_bytes()))
        }
        (Some(body), None) if body.get().starts_with('{') => {
            Ok(Bytes::copy_from_slice(body.get().as_bytes()))
        }
        _ => Err(sent(Kind::Protocol)),
    }
}
