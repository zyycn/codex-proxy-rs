use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use url::Url;

use crate::{SecretValue, VerificationFailure};

/// 正在验证首个 access token 的 OAuth 流程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationFlow {
    /// Authorization Code + PKCE；必须携带与 nonce 绑定的 ID token。
    AuthorizationCode,
    /// 已有 token 的导入；必须通过官方 user-info 验证当前 access token。
    CredentialImport,
    /// 导入的 AT 已过期并经官方 RT exchange 更新；必须验证官方 user-info。
    CredentialImportRefreshed,
}

/// 注入的校验器报告的可信验证机制。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMethod {
    /// 完整 JWT/JWKS 校验，含 issuer、audience、过期时间与 nonce。
    IdToken,
    /// 用 access token 查询官方权威 user-info。
    UserInfo,
}

/// 可信校验器产出的验证证据。身份在 debug 输出中脱敏，且本 crate 不会通过
/// 未经验证的 JWT 解码推导身份。
#[derive(Clone)]
pub struct VerificationEvidence {
    method: VerificationMethod,
    subject: SecretValue,
}

impl VerificationEvidence {
    /// 完整 ID token 校验通过后创建证据。
    #[must_use]
    pub fn id_token(subject: String) -> Self {
        Self {
            method: VerificationMethod::IdToken,
            subject: SecretValue::new(subject),
        }
    }

    /// 官方权威 user-info 查询通过后创建证据。
    #[must_use]
    pub fn user_info(subject: String) -> Self {
        Self {
            method: VerificationMethod::UserInfo,
            subject: SecretValue::new(subject),
        }
    }

    /// 返回验证机制。
    #[must_use]
    pub const fn method(&self) -> VerificationMethod {
        self.method
    }

    /// 在凭据构造边界暴露已验证的 subject。
    #[must_use]
    pub fn subject(&self) -> &str {
        self.subject.expose()
    }
}

impl fmt::Debug for VerificationEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationEvidence")
            .field("method", &self.method)
            .field("subject", &"[REDACTED]")
            .finish()
    }
}

/// 传给 token 校验器的不可变信任上下文。
#[derive(Debug, Clone, Copy)]
pub struct TokenVerificationContext<'a> {
    flow: VerificationFlow,
    issuer: &'a Url,
    client_id: &'a str,
    jwks_uri: &'a Url,
    userinfo_endpoint: &'a Url,
    signing_algorithms: &'a [String],
    expected_nonce: Option<&'a SecretValue>,
}

impl<'a> TokenVerificationContext<'a> {
    pub fn new(
        flow: VerificationFlow,
        issuer: &'a Url,
        client_id: &'a str,
        jwks_uri: &'a Url,
        userinfo_endpoint: &'a Url,
        signing_algorithms: &'a [String],
        expected_nonce: Option<&'a SecretValue>,
    ) -> Self {
        Self {
            flow,
            issuer,
            client_id,
            jwks_uri,
            userinfo_endpoint,
            signing_algorithms,
            expected_nonce,
        }
    }

    /// 返回正在验证的流程。
    #[must_use]
    pub const fn flow(&self) -> VerificationFlow {
        self.flow
    }

    /// 返回精确匹配的预期 issuer。
    #[must_use]
    pub fn issuer(&self) -> &Url {
        self.issuer
    }

    /// 返回精确匹配的预期 audience/client 标识。
    #[must_use]
    pub const fn client_id(&self) -> &str {
        self.client_id
    }

    /// 返回已校验发现文档中的同源 JWKS URL。
    #[must_use]
    pub fn jwks_uri(&self) -> &Url {
        self.jwks_uri
    }

    /// 返回发现文档中的同源权威 user-info URL。
    #[must_use]
    pub fn userinfo_endpoint(&self) -> &Url {
        self.userinfo_endpoint
    }

    /// 返回发现文档声明的算法。实现必须与本地密码学 allowlist 取交集，并拒绝
    /// `none`。
    #[must_use]
    pub const fn signing_algorithms(&self) -> &[String] {
        self.signing_algorithms
    }

    /// 返回 Authorization Code 流程的预期 nonce。
    #[must_use]
    pub const fn expected_nonce(&self) -> Option<&SecretValue> {
        self.expected_nonce
    }
}

/// 仅向可信校验端口开放的借用 token 材料。
pub struct TokenCandidate<'a> {
    access_token: &'a SecretValue,
    id_token: Option<&'a SecretValue>,
    expires_in: Option<Duration>,
}

impl<'a> TokenCandidate<'a> {
    pub const fn new(
        access_token: &'a SecretValue,
        id_token: Option<&'a SecretValue>,
        expires_in: Option<Duration>,
    ) -> Self {
        Self {
            access_token,
            id_token,
            expires_in,
        }
    }

    /// 返回用于权威 user-info 验证的 access token。
    #[must_use]
    pub const fn access_token(&self) -> &SecretValue {
        self.access_token
    }

    /// 返回用于完整 JWT/JWKS 校验的 ID token（若存在）。
    #[must_use]
    pub const fn id_token(&self) -> Option<&SecretValue> {
        self.id_token
    }

    /// 返回服务端给出的有效期。
    #[must_use]
    pub const fn expires_in(&self) -> Option<Duration> {
        self.expires_in
    }
}

impl fmt::Debug for TokenCandidate<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenCandidate")
            .field("access_token", &"[REDACTED]")
            .field("id_token", &self.id_token.map(|_| "[REDACTED]"))
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// token 校验端口返回的 future。
pub type VerificationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<VerificationEvidence, VerificationFailure>> + Send + 'a>>;

/// 完整 JWT/JWKS 或权威 user-info 校验的信任边界。
pub trait TokenVerifier: Send + Sync {
    /// 验证初始 token 集合。实现不得在未做签名与 claim 校验的情况下 base64 解码
    /// JWT claims。
    fn verify<'a>(
        &'a self,
        context: TokenVerificationContext<'a>,
        candidate: TokenCandidate<'a>,
    ) -> VerificationFuture<'a>;
}

/// 默认校验器，未完成接线时 fail closed。
#[derive(Debug, Default)]
pub struct FailClosedTokenVerifier;

impl TokenVerifier for FailClosedTokenVerifier {
    fn verify<'a>(
        &'a self,
        _context: TokenVerificationContext<'a>,
        _candidate: TokenCandidate<'a>,
    ) -> VerificationFuture<'a> {
        Box::pin(async { Err(VerificationFailure::Unavailable) })
    }
}
