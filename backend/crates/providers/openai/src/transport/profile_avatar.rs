//! Codex 官方个人资料头像的固定来源流式 transport。

use std::{pin::Pin, time::Duration};

use async_stream::try_stream;
use bytes::Bytes;
use futures::{Stream, StreamExt as _};
use reqwest::{
    Client, Url,
    header::{ACCEPT, CONTENT_TYPE, ETAG, USER_AGENT},
};
use tokio::time::timeout;

use super::endpoints::endpoint_url;

const OFFICIAL_AVATAR_ORIGIN: &str = "https://chatgpt.com";
const OFFICIAL_AVATAR_PATH_PREFIX: &str = "/backend-api/estuary/public_content/enc/";
const PROVIDER_AVATAR_PATH_PREFIX: &str = "/estuary/public_content/enc/";
const PROFILE_AVATAR_ACCEPT: &str =
    "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8";
const PROFILE_AVATAR_HEADERS_TIMEOUT: Duration = Duration::from_secs(15);
const PROFILE_AVATAR_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// 头像正文流；不累积完整正文，也不设置总字节数上限。
pub type CodexProfileAvatarStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, CodexProfileAvatarStreamError>> + Send + 'static>>;

/// 响应开始后的头像流失败；不得携带带签名的上游 URL。
#[derive(Debug, thiserror::Error)]
#[error("Codex profile avatar stream failed")]
pub struct CodexProfileAvatarStreamError;

/// 已打开的官方头像响应；MIME 只透传，不做格式白名单判断。
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

/// 校验官方来源后，通过当前 Provider base URL 打开头像字节流。
///
/// # Errors
///
/// 来源不是固定的 ChatGPT Estuary 路径、请求失败或上游返回非成功状态时返回错误。
pub async fn fetch_profile_avatar(
    client: &Client,
    base_url: &str,
    desktop_user_agent: &str,
    source: &str,
) -> Result<CodexProfileAvatar, CodexProfileAvatarFetchError> {
    let source_path = official_avatar_source_path(source)?;
    let target = Url::parse(&endpoint_url(base_url, &source_path))
        .map_err(|_| CodexProfileAvatarFetchError::TransportUnavailable)?;
    let response = timeout(
        PROFILE_AVATAR_HEADERS_TIMEOUT,
        client
            .get(target)
            .header(ACCEPT, PROFILE_AVATAR_ACCEPT)
            .header(USER_AGENT, desktop_user_agent)
            .send(),
    )
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

fn official_avatar_source_path(source: &str) -> Result<String, CodexProfileAvatarFetchError> {
    let source = Url::parse(source).map_err(|_| CodexProfileAvatarFetchError::InvalidSource)?;
    if source.origin().ascii_serialization() != OFFICIAL_AVATAR_ORIGIN
        || !source.username().is_empty()
        || source.password().is_some()
        || source.query().is_some()
        || source.fragment().is_some()
    {
        return Err(CodexProfileAvatarFetchError::InvalidSource);
    }
    let opaque_path = source
        .path()
        .strip_prefix(OFFICIAL_AVATAR_PATH_PREFIX)
        .filter(|path| !path.is_empty())
        .ok_or(CodexProfileAvatarFetchError::InvalidSource)?;
    Ok(format!("{PROVIDER_AVATAR_PATH_PREFIX}{opaque_path}"))
}
