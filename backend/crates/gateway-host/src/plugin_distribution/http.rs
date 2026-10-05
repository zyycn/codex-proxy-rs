//! 插件制品下载的来源校验、凭据匹配与限流错误处理

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gateway_admin::model::{
    AdminError, AdminErrorKind,
    plugins::distribution::{
        DownloadPurpose, PluginDistributionEgress, SourceAuthentication, SourceCredential,
    },
};
use reqwest::{
    Url,
    header::{HeaderName, HeaderValue},
};
use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex;

use super::source_error;
use crate::outbound::{HttpClient, HttpRequest, NetworkPolicy};

pub(super) struct Downloads {
    client: Arc<HttpClient>,
    network: NetworkPolicy,
    rate_limits: Mutex<HashMap<String, Instant>>,
}

impl Downloads {
    pub(super) fn new(client: Arc<HttpClient>, network: NetworkPolicy) -> Self {
        Self {
            client,
            network,
            rate_limits: Mutex::new(HashMap::new()),
        }
    }

    pub(super) async fn get(
        &self,
        mut url: Url,
        purpose: DownloadPurpose,
        credentials: &[SourceCredential],
        limit: usize,
        github_api: bool,
        egress: Option<&PluginDistributionEgress>,
    ) -> Result<Vec<u8>, AdminError> {
        for redirect in 0..=5 {
            validate_url(&url)?;
            let egress_key =
                egress.map_or_else(|| "direct".to_owned(), PluginDistributionEgress::cache_key);
            let scope = format!("{}:{egress_key}", scope_key(&url, purpose, credentials)?);
            {
                let mut rates = self.rate_limits.lock().await;
                rates.retain(|_, until| *until > Instant::now());
                if let Some(until) = rates.get(&scope) {
                    return Err(rate_error(
                        until
                            .saturating_duration_since(Instant::now())
                            .as_secs()
                            .max(1),
                    ));
                }
                if rates.len() >= 256 {
                    return Err(AdminError::unavailable("插件来源退避列表已满，请稍后重试"));
                }
            }
            let mut headers = vec![(
                "user-agent".to_owned(),
                b"codex-proxy-rs-plugin-distribution".to_vec(),
            )];
            if github_api {
                headers.extend([
                    (
                        "accept".to_owned(),
                        if purpose == DownloadPurpose::Metadata {
                            b"application/vnd.github+json".to_vec()
                        } else {
                            b"application/octet-stream".to_vec()
                        },
                    ),
                    ("x-github-api-version".to_owned(), b"2022-11-28".to_vec()),
                ]);
            }
            if let Some(credential) = matching_credential(&url, purpose, credentials)? {
                let (name, value) = authentication_header(&credential.authentication)?;
                headers.push((name.to_string(), value.as_bytes().to_vec()));
            }
            let mut response = self
                .client
                .open_scoped(
                    HttpRequest {
                        method: "GET".to_owned(),
                        url: url.to_string(),
                        headers,
                        body: vec![],
                    },
                    egress.map(|egress| &egress.proxy),
                    &self.network,
                    Duration::from_secs(30),
                    Some(&egress_key),
                )
                .await
                .map_err(|_| source_error())?;
            if let Some(delay) = rate_delay(response.status, &response.headers) {
                let mut rates = self.rate_limits.lock().await;
                // 限流项有界；超出时拒绝新查询，不淘汰仍然有效的退避
                if rates.len() < 256 || rates.contains_key(&scope) {
                    rates.insert(scope, Instant::now() + delay);
                }
                return Err(rate_error(delay.as_secs()));
            }
            if (300..400).contains(&response.status) {
                if redirect == 5 {
                    return Err(source_error());
                }
                let location = response
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("location"))
                    .and_then(|(_, value)| std::str::from_utf8(value).ok())
                    .ok_or_else(source_error)?;
                let next = url.join(location).map_err(|_| source_error())?;
                if url.scheme() == "https" && next.scheme() != "https" {
                    return Err(source_error());
                }
                // 不继承上一次请求的头；下一跳从授权列表重新匹配，包括同域的路径变化
                url = next;
                continue;
            }
            if !(200..300).contains(&response.status) {
                tracing::warn!(status = response.status, "插件来源返回非成功状态");
                return Err(source_error());
            }
            if header(&response.headers, "content-length")
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|length| length > limit as u64)
            {
                return Err(AdminError::invalid("插件来源响应超过大小限制"));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .body
                .read(64 * 1024)
                .await
                .map_err(|_| source_error())?
            {
                if chunk.len() > limit.saturating_sub(body.len()) {
                    return Err(AdminError::invalid("插件来源响应超过大小限制"));
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(body);
        }
        Err(source_error())
    }
}

pub(super) fn source_url(value: &str) -> Result<Url, AdminError> {
    let url = Url::parse(value).map_err(|_| AdminError::invalid("插件来源 URL 不合法"))?;
    validate_url(&url)?;
    if url.query().is_some() {
        return Err(AdminError::invalid(
            "插件来源 URL 不接受查询参数，请使用独立下载凭据",
        ));
    }
    Ok(url)
}

fn validate_url(url: &Url) -> Result<(), AdminError> {
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if url.as_str().len() > 4096
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        return Err(AdminError::invalid(
            "插件来源需要 HTTPS，且不能在 URL 中包含凭据或片段",
        ));
    }
    Ok(())
}

pub(super) fn validate_credential(credential: &SourceCredential) -> Result<(), AdminError> {
    let info = &credential.info;
    let origin = source_url(&info.origin)?;
    if origin.path() != "/"
        || info.name.trim().is_empty()
        || info.name.len() > 128
        || !info.path_prefix.starts_with('/')
        || info.path_prefix.len() > 1024
        || info.path_prefix.contains(['?', '#', '%', '\\'])
        || info
            .path_prefix
            .split('/')
            .any(|part| part == "." || part == "..")
        || info.purposes.is_empty()
        || info.purposes.len() > 2
        || (info.purposes.len() == 2 && info.purposes[0] == info.purposes[1])
    {
        return Err(AdminError::invalid("下载凭据的目标、路径或用途不合法"));
    }
    authentication_header(&credential.authentication)?;
    Ok(())
}

fn matching_credential<'a>(
    url: &Url,
    purpose: DownloadPurpose,
    credentials: &'a [SourceCredential],
) -> Result<Option<&'a SourceCredential>, AdminError> {
    let mut selected = None;
    for credential in credentials {
        validate_credential(credential)?;
        let origin = source_url(&credential.info.origin)?;
        let prefix = credential.info.path_prefix.trim_end_matches('/');
        if url.origin() == origin.origin()
            && credential.info.purposes.contains(&purpose)
            && (prefix.is_empty()
                || url.path() == prefix
                || url
                    .path()
                    .strip_prefix(prefix)
                    .is_some_and(|suffix| suffix.starts_with('/')))
        {
            if selected.is_some() {
                return Err(AdminError::invalid("下载凭据的授权范围重叠"));
            }
            selected = Some(credential);
        }
    }
    Ok(selected)
}

pub(super) fn scope_key(
    url: &Url,
    purpose: DownloadPurpose,
    credentials: &[SourceCredential],
) -> Result<String, AdminError> {
    let mut digest = Sha256::new();
    digest.update(url.origin().ascii_serialization());
    if let Some(credential) = matching_credential(url, purpose, credentials)? {
        let (name, value) = authentication_header(&credential.authentication)?;
        digest.update(name.as_str());
        digest.update(value.as_bytes());
    }
    Ok(hex::encode(digest.finalize()))
}

fn authentication_header(
    authentication: &SourceAuthentication,
) -> Result<(HeaderName, HeaderValue), AdminError> {
    use base64::Engine as _;
    let (name, raw) = match authentication {
        SourceAuthentication::Github { token } | SourceAuthentication::Bearer { token } => {
            if token.expose_secret().trim().is_empty() {
                return Err(AdminError::invalid("下载 token 不能为空"));
            }
            (
                "authorization".to_owned(),
                format!("Bearer {}", token.expose_secret()),
            )
        }
        SourceAuthentication::Basic { username, password } => {
            if username.contains(':') || username.len() > 256 {
                return Err(AdminError::invalid("Basic 用户名不合法"));
            }
            (
                "authorization".to_owned(),
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{username}:{}", password.expose_secret()))
                ),
            )
        }
        SourceAuthentication::Header { name, value } => {
            (name.to_ascii_lowercase(), value.expose_secret().to_owned())
        }
    };
    if raw.trim().is_empty()
        || raw.len() > 8192
        || !(name == "authorization"
            || (name.starts_with("x-")
                && !name.starts_with("x-forwarded-")
                && name != "x-http-method-override"))
    {
        return Err(AdminError::invalid("下载鉴权头不合法"));
    }
    let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| AdminError::invalid("下载鉴权头不合法"))?;
    let mut value =
        HeaderValue::from_str(&raw).map_err(|_| AdminError::invalid("下载鉴权值不合法"))?;
    value.set_sensitive(true);
    Ok((name, value))
}

fn header<'a>(headers: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a [u8]> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_slice())
}

fn rate_delay(status: u16, headers: &[(String, Vec<u8>)]) -> Option<Duration> {
    if ![403, 429].contains(&status) {
        return None;
    }
    let retry = header(headers, "retry-after")
        .and_then(|value| std::str::from_utf8(value).ok())
        .and_then(|value| value.parse::<u64>().ok());
    let remaining =
        header(headers, "x-ratelimit-remaining").and_then(|value| std::str::from_utf8(value).ok());
    let reset = (remaining == Some("0"))
        .then(|| {
            header(headers, "x-ratelimit-reset")
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(|value| value.parse::<u64>().ok())
        })
        .flatten();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Some(Duration::from_secs(
        retry
            .or_else(|| reset.map(|until| until.saturating_sub(now)))
            .unwrap_or(60)
            .clamp(1, 86400),
    ))
}

fn rate_error(seconds: u64) -> AdminError {
    AdminError::new(
        AdminErrorKind::RateLimited,
        format!("插件来源限流，请在 {seconds} 秒后重试"),
    )
}
