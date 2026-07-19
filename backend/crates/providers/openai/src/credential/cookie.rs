use std::collections::HashSet;

use chrono::{DateTime, TimeDelta, Utc};
use cookie::Cookie;
use secrecy::SecretString;
use url::Url;

use super::types::UpsertCodexCookie;

const MAX_SET_COOKIE_HEADERS: usize = 32;
const MAX_SET_COOKIE_HEADER_BYTES: usize = 16 * 1024;
const MAX_SET_COOKIE_TOTAL_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct CodexCookiePolicy {
    allowed_names: HashSet<String>,
    allowed_domains: HashSet<String>,
}

impl CodexCookiePolicy {
    pub fn new(
        allowed_names: impl IntoIterator<Item = impl Into<String>>,
        allowed_domains: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, CookiePolicyError> {
        let allowed_names = allowed_names
            .into_iter()
            .map(Into::into)
            .collect::<HashSet<_>>();
        let allowed_domains = allowed_domains
            .into_iter()
            .map(Into::into)
            .map(|domain| normalize_domain(&domain))
            .collect::<Result<HashSet<_>, _>>()?;
        if allowed_names.is_empty() || allowed_domains.is_empty() {
            return Err(CookiePolicyError::EmptyAllowlist);
        }
        Ok(Self {
            allowed_names,
            allowed_domains,
        })
    }

    pub fn official() -> Result<Self, CookiePolicyError> {
        Self::new(
            [
                "__Secure-next-auth.session-token",
                "__Secure-authjs.session-token",
                "oai-did",
                "cf_clearance",
                "__cf_bm",
                "_cfuvid",
            ],
            ["chatgpt.com", "openai.com"],
        )
    }

    pub fn validate_capture(
        &self,
        origin: &Url,
        domain_attribute: Option<&str>,
        name: &str,
        path: &str,
    ) -> Result<ValidatedCookieScope, CookiePolicyError> {
        if !self.allowed_names.contains(name) {
            return Err(CookiePolicyError::NameNotAllowed);
        }
        if name.len() > 256 || path.is_empty() || path.len() > 1_024 || !path.starts_with('/') {
            return Err(CookiePolicyError::InvalidScope);
        }
        let origin_host = origin
            .host_str()
            .map(normalize_domain)
            .transpose()?
            .ok_or(CookiePolicyError::InvalidOrigin)?;
        if !matches!(origin.scheme(), "https" | "http") || !self.is_allowed_domain(&origin_host) {
            return Err(CookiePolicyError::InvalidOrigin);
        }

        match domain_attribute {
            Some(domain_attribute) => {
                let domain = normalize_domain(domain_attribute)?;
                if !self.is_allowed_domain(&domain) || !domain_matches(&origin_host, &domain) {
                    return Err(CookiePolicyError::InvalidScope);
                }
                Ok(ValidatedCookieScope {
                    domain,
                    host_only: false,
                })
            }
            None => Ok(ValidatedCookieScope {
                domain: origin_host,
                host_only: true,
            }),
        }
    }

    pub fn may_replay(
        &self,
        target: &Url,
        domain: &str,
        path: &str,
        host_only: bool,
        secure: bool,
    ) -> bool {
        let Some(target_host) = target.host_str() else {
            return false;
        };
        let Ok(target_host) = normalize_domain(target_host) else {
            return false;
        };
        if !self.is_allowed_domain(&target_host)
            || (secure && target.scheme() != "https")
            || !cookie_path_matches(target.path(), path)
        {
            return false;
        }
        if host_only {
            target_host == domain
        } else {
            domain_matches(&target_host, domain)
        }
    }

    pub(crate) fn parse_response_headers(
        &self,
        account_id: &str,
        expected_credential_revision: u64,
        response_origin: &Url,
        headers: &[String],
        observed_at: DateTime<Utc>,
    ) -> ParsedCookieBatch {
        let total_bytes = headers
            .iter()
            .try_fold(0_usize, |total, header| total.checked_add(header.len()));
        if headers.len() > MAX_SET_COOKIE_HEADERS
            || total_bytes.is_none_or(|total| total > MAX_SET_COOKIE_TOTAL_BYTES)
        {
            return ParsedCookieBatch {
                inputs: Vec::new(),
                rejected: headers.len(),
            };
        }

        let mut inputs = Vec::with_capacity(headers.len());
        let mut rejected = 0;
        for header in headers {
            let Some(input) = self.parse_response_header(
                account_id,
                expected_credential_revision,
                response_origin,
                header,
                observed_at,
            ) else {
                rejected += 1;
                continue;
            };
            inputs.push(input);
        }
        ParsedCookieBatch { inputs, rejected }
    }

    fn parse_response_header(
        &self,
        account_id: &str,
        expected_credential_revision: u64,
        response_origin: &Url,
        header: &str,
        observed_at: DateTime<Utc>,
    ) -> Option<UpsertCodexCookie> {
        if header.is_empty() || header.len() > MAX_SET_COOKIE_HEADER_BYTES {
            return None;
        }
        let cookie = Cookie::parse(header.to_owned()).ok()?;
        let path = cookie
            .path()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| default_cookie_path(response_origin.path()));
        self.validate_capture(response_origin, cookie.domain(), cookie.name(), &path)
            .ok()?;
        let expires_at = cookie_expiry(&cookie, observed_at)?;
        let delete = expires_at.is_some_and(|expires_at| expires_at <= observed_at);
        if cookie.value().is_empty() && !delete {
            return None;
        }
        Some(UpsertCodexCookie {
            account_id: account_id.to_owned(),
            expected_credential_revision,
            response_origin: response_origin.clone(),
            domain_attribute: cookie.domain().map(ToOwned::to_owned),
            name: cookie.name().to_owned(),
            value: SecretString::new(cookie.value().to_owned().into()),
            path,
            secure: cookie.secure().unwrap_or(false),
            expires_at,
            delete,
        })
    }

    fn is_allowed_domain(&self, domain: &str) -> bool {
        self.allowed_domains
            .iter()
            .any(|allowed| domain_matches(domain, allowed))
    }
}

pub(crate) struct ParsedCookieBatch {
    pub(crate) inputs: Vec<UpsertCodexCookie>,
    pub(crate) rejected: usize,
}

fn cookie_expiry(cookie: &Cookie<'_>, observed_at: DateTime<Utc>) -> Option<Option<DateTime<Utc>>> {
    if let Some(max_age) = cookie.max_age() {
        let seconds = max_age.whole_seconds();
        return observed_at
            .checked_add_signed(TimeDelta::try_seconds(seconds)?)
            .map(Some);
    }
    let Some(expires) = cookie.expires_datetime() else {
        return Some(None);
    };
    DateTime::from_timestamp(expires.unix_timestamp(), expires.nanosecond()).map(Some)
}

fn default_cookie_path(request_path: &str) -> String {
    if !request_path.starts_with('/') || request_path == "/" {
        return "/".to_owned();
    }
    let Some(last_slash) = request_path.rfind('/') else {
        return "/".to_owned();
    };
    if last_slash == 0 {
        "/".to_owned()
    } else {
        request_path[..last_slash].to_owned()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CookiePolicyError {
    #[error("cookie allowlist must contain at least one name and domain")]
    EmptyAllowlist,
    #[error("cookie name is not allowed by the Codex provider")]
    NameNotAllowed,
    #[error("cookie response origin is not allowed by the Codex provider")]
    InvalidOrigin,
    #[error("cookie scope is invalid for its response origin")]
    InvalidScope,
}

pub struct ValidatedCookieScope {
    pub(crate) domain: String,
    pub(crate) host_only: bool,
}

fn normalize_domain(domain: &str) -> Result<String, CookiePolicyError> {
    let domain = domain
        .trim()
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if domain.is_empty()
        || domain.len() > 253
        || domain.contains('/')
        || domain.contains(':')
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(CookiePolicyError::InvalidScope);
    }
    Ok(domain)
}

fn domain_matches(host: &str, cookie_domain: &str) -> bool {
    host == cookie_domain
        || host
            .strip_suffix(cookie_domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn cookie_path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || request_path
            .strip_prefix(cookie_path)
            .is_some_and(|suffix| cookie_path.ends_with('/') || suffix.starts_with('/'))
}
