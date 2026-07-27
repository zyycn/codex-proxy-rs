use std::fmt;

use url::{Host, Url};

use crate::ConfigError;

/// Grok Build 使用的官方第一方 OIDC issuer。
pub const OFFICIAL_ISSUER: &str = "https://auth.x.ai";

/// Grok Build 内置的官方 public OAuth client 标识。
pub const OFFICIAL_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// 官方桌面客户端使用的固定 loopback OAuth 回调地址。
pub const OFFICIAL_REDIRECT_URI: &str = "http://127.0.0.1:56121/callback";

/// 当前 Grok Build 客户端使用的官方第一方 OAuth scope 集合。
pub const OFFICIAL_SCOPES: &[&str] = &[
    "openid",
    "profile",
    "email",
    "offline_access",
    "grok-cli:access",
    "api:access",
];

/// 已通过语法与本地 allowlist 校验的精确回调 URI。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedRedirectUri(Url);

impl AllowedRedirectUri {
    /// 返回授权与 token 交换时发送的精确回调 URI。
    #[must_use]
    pub fn as_url(&self) -> &Url {
        &self.0
    }

    pub(crate) fn restore_server_side(value: &str) -> Result<Self, ConfigError> {
        validate_redirect_uri(value).map(Self)
    }
}

/// 显式回调 allowlist。公网 HTTPS 回调与动态 loopback 回调必须先登记才能启动流程。
#[derive(Debug, Clone)]
pub struct RedirectUriAllowlist {
    entries: Vec<Url>,
}

impl RedirectUriAllowlist {
    /// 校验并构造精确回调 allowlist。
    ///
    /// # Errors
    ///
    /// 公网 URI 非 HTTPS、含 user-info、query、fragment 或格式非法时返回
    /// [`ConfigError::InvalidRedirectUri`]。
    pub fn new<I, S>(uris: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let entries = uris
            .into_iter()
            .map(|uri| validate_redirect_uri(uri.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { entries })
    }

    /// 校验候选 URI，并要求与本地 allowlist 精确匹配。
    ///
    /// # Errors
    ///
    /// URI 非法或未登记时返回配置错误。
    pub fn authorize(&self, candidate: &str) -> Result<AllowedRedirectUri, ConfigError> {
        let candidate = validate_redirect_uri(candidate)?;
        if self.entries.iter().any(|entry| entry == &candidate) {
            return Ok(AllowedRedirectUri(candidate));
        }

        Err(ConfigError::RedirectUriNotAllowlisted)
    }
}

/// 不可变的官方 Grok Build OAuth 配置。
#[derive(Clone)]
pub struct GrokOAuthConfig {
    issuer: Url,
}

impl GrokOAuthConfig {
    /// 构造固定的官方 issuer、client 与 scope 配置。
    ///
    /// # Errors
    ///
    /// 固定官方 issuer 无法构造时返回错误。
    pub fn official() -> Result<Self, ConfigError> {
        let issuer = Url::parse(OFFICIAL_ISSUER).map_err(|_| ConfigError::UntrustedIssuer)?;
        Ok(Self { issuer })
    }

    /// 返回固定的 issuer URL。
    #[must_use]
    pub fn issuer(&self) -> &Url {
        &self.issuer
    }

    /// 返回官方 public client 标识。
    #[must_use]
    pub const fn client_id(&self) -> &'static str {
        OFFICIAL_CLIENT_ID
    }

    /// 返回官方 scope 集合。
    #[must_use]
    pub const fn scopes(&self) -> &'static [&'static str] {
        OFFICIAL_SCOPES
    }

    pub(crate) fn scope_string(&self) -> String {
        OFFICIAL_SCOPES.join(" ")
    }

    pub(crate) fn discovery_url(&self) -> Url {
        endpoint_from_issuer(&self.issuer, "/.well-known/openid-configuration")
    }

    pub fn validate_discovered_endpoint(&self, value: &str) -> Result<Url, ConfigError> {
        let endpoint = Url::parse(value).map_err(|_| ConfigError::UntrustedEndpoint)?;
        if endpoint.scheme() != "https"
            || endpoint.host_str() != self.issuer.host_str()
            || endpoint.port_or_known_default() != Some(443)
            || endpoint.path() == "/"
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ConfigError::UntrustedEndpoint);
        }

        Ok(endpoint)
    }

    pub fn validate_discovered_issuer(&self, value: &str) -> Result<Url, ConfigError> {
        let issuer = Url::parse(value).map_err(|_| ConfigError::UntrustedIssuer)?;
        if normalized_origin_and_path(&issuer) != normalized_origin_and_path(&self.issuer)
            || !issuer.username().is_empty()
            || issuer.password().is_some()
            || issuer.query().is_some()
            || issuer.fragment().is_some()
        {
            return Err(ConfigError::UntrustedIssuer);
        }

        Ok(issuer)
    }
}

impl fmt::Debug for GrokOAuthConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokOAuthConfig")
            .field("issuer", &self.issuer)
            .field("client_id", &OFFICIAL_CLIENT_ID)
            .field("scopes", &OFFICIAL_SCOPES)
            .finish()
    }
}

fn validate_redirect_uri(value: &str) -> Result<Url, ConfigError> {
    let uri = Url::parse(value).map_err(|_| ConfigError::InvalidRedirectUri)?;
    let is_loopback_host = match uri.host() {
        Some(Host::Domain("localhost")) => true,
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(_)) | None => false,
    };
    let is_loopback_http = uri.scheme() == "http" && is_loopback_host;
    let is_secure_public = uri.scheme() == "https" && uri.host().is_some();

    if !(is_loopback_http || is_secure_public)
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.query().is_some()
        || uri.fragment().is_some()
    {
        return Err(ConfigError::InvalidRedirectUri);
    }

    Ok(uri)
}

fn endpoint_from_issuer(issuer: &Url, path: &str) -> Url {
    let mut endpoint = issuer.clone();
    endpoint.set_path(path);
    endpoint
}

fn normalized_origin_and_path(url: &Url) -> (&str, Option<&str>, Option<u16>, &str) {
    (
        url.scheme(),
        url.host_str(),
        url.port_or_known_default(),
        url.path().trim_end_matches('/'),
    )
}
