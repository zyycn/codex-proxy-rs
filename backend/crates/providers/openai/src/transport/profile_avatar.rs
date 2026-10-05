//! Codex 官方个人资料头像的固定来源流式 transport

use std::{pin::Pin, time::Duration};

use async_stream::try_stream;
use bytes::Bytes;
use futures::{Stream, StreamExt as _};
use reqwest::{
    Client, Request, Url,
    header::{CONTENT_TYPE, ETAG, USER_AGENT},
};
use tokio::time::timeout;

use super::endpoints::endpoint_url;
use super::{
    CodexRequestContext, headers::build_codex_download_headers, profile::CodexWireProfile,
};

const OFFICIAL_AVATAR_ORIGIN: &str = "https://chatgpt.com";
const OFFICIAL_AVATAR_PATH_PREFIX: &str = "/backend-api/estuary/public_content/enc/";
const PROVIDER_AVATAR_PATH_PREFIX: &str = "/estuary/public_content/enc/";
const AUTH0_AVATAR_ORIGIN: &str = "https://cdn.auth0.com";
const AUTH0_AVATAR_PATH_PREFIX: &str = "/avatars/";
const PROFILE_AVATAR_HEADERS_TIMEOUT: Duration = Duration::from_secs(15);
const PROFILE_AVATAR_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// 头像正文流；不累积完整正文，也不设置总字节数上限
pub type CodexProfileAvatarStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, CodexProfileAvatarStreamError>> + Send + 'static>>;

/// 响应开始后的头像流失败；不得携带带签名的上游 URL
#[derive(Debug, thiserror::Error)]
#[error("Codex profile avatar stream failed")]
pub struct CodexProfileAvatarStreamError;

/// 已打开的官方头像响应；MIME 只透传，不做格式白名单判断
pub struct CodexProfileAvatar {
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub etag: Option<String>,
    pub body: CodexProfileAvatarStream,
}

impl std::fmt::Debug for CodexProfileAvatar {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexProfileAvatar")
            .field("content_type", &self.content_type)
            .field("content_length", &self.content_length)
            .field("etag", &self.etag)
            .field("body", &"<stream>")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodexProfileAvatarFetchError {
    #[error("Codex profile avatar source is invalid")]
    InvalidSource,
    #[error("Codex profile avatar upstream returned HTTP {status}")]
    Upstream { status: u16 },
    #[error("Codex profile avatar transport is unavailable")]
    TransportUnavailable,
}

/// 校验来源并构造头像请求；账号凭据仅用于 ChatGPT Estuary
///
/// # Errors
///
/// 来源不受支持或请求头无法编码时返回脱敏错误
pub fn build_profile_avatar_request(
    client: &Client,
    base_url: &str,
    profile: &CodexWireProfile,
    source: &str,
    context: CodexRequestContext<'_>,
) -> Result<Request, CodexProfileAvatarFetchError> {
    let source = Url::parse(source).map_err(|_| CodexProfileAvatarFetchError::InvalidSource)?;
    if !source.username().is_empty()
        || source.password().is_some()
        || source.query().is_some()
        || source.fragment().is_some()
    {
        return Err(CodexProfileAvatarFetchError::InvalidSource);
    }
    let request = match source.origin().ascii_serialization().as_str() {
        OFFICIAL_AVATAR_ORIGIN => {
            let opaque_path = avatar_path(&source, OFFICIAL_AVATAR_PATH_PREFIX)?;
            let target = endpoint_url(
                base_url,
                &format!("{PROVIDER_AVATAR_PATH_PREFIX}{opaque_path}"),
            );
            let headers =
                build_codex_download_headers(profile, context.authorization, context.account_id)
                    .map_err(|_| CodexProfileAvatarFetchError::TransportUnavailable)?;
            client.get(target).headers(headers)
        }
        AUTH0_AVATAR_ORIGIN => {
            avatar_path(&source, AUTH0_AVATAR_PATH_PREFIX)?;
            // Auth0 的默认头像是公开 CDN 资源，不能携带 ChatGPT 账号凭据
            client
                .get(source)
                .header(USER_AGENT, profile.desktop_user_agent())
        }
        _ => return Err(CodexProfileAvatarFetchError::InvalidSource),
    };
    request
        .build()
        .map_err(|_| CodexProfileAvatarFetchError::TransportUnavailable)
}

/// 打开已校验来源的头像字节流
///
/// # Errors
///
/// 来源不受支持、请求失败或上游返回非成功状态时返回错误
pub async fn fetch_profile_avatar(
    client: &Client,
    base_url: &str,
    profile: &CodexWireProfile,
    source: &str,
    context: CodexRequestContext<'_>,
) -> Result<CodexProfileAvatar, CodexProfileAvatarFetchError> {
    let request = build_profile_avatar_request(client, base_url, profile, source, context)?;
    let response = timeout(PROFILE_AVATAR_HEADERS_TIMEOUT, client.execute(request))
        .await
        .map_err(|_| CodexProfileAvatarFetchError::TransportUnavailable)?
        .map_err(|_| CodexProfileAvatarFetchError::TransportUnavailable)?;
    let status = response.status();
    if !status.is_success() {
        return Err(CodexProfileAvatarFetchError::Upstream {
            status: status.as_u16(),
        });
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let content_length = response.content_length();
    let mut upstream = Box::pin(response.bytes_stream());
    let body = try_stream! {
        loop {
            match timeout(PROFILE_AVATAR_STREAM_IDLE_TIMEOUT, upstream.next()).await {
                Ok(Some(Ok(chunk))) => yield chunk,
                Ok(Some(Err(_))) | Err(_) => Err(CodexProfileAvatarStreamError)?,
                Ok(None) => break,
            }
        }
    };

    Ok(CodexProfileAvatar {
        content_type,
        content_length,
        etag,
        body: Box::pin(body),
    })
}

fn avatar_path<'a>(source: &'a Url, prefix: &str) -> Result<&'a str, CodexProfileAvatarFetchError> {
    source
        .path()
        .strip_prefix(prefix)
        .filter(|path| !path.is_empty())
        .ok_or(CodexProfileAvatarFetchError::InvalidSource)
}
