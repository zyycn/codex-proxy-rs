use thiserror::Error;

use crate::TransportFailureKind;

/// OAuth 操作标签，用于脱敏遥测与错误分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthOperation {
    /// OpenID Provider 发现。
    Discovery,
    /// Authorization Code 换取 token。
    AuthorizationCodeToken,
    /// Refresh token 换取 token。
    RefreshToken,
    /// 已有 OAuth token 的受控导入验证。
    CredentialImport,
}

/// 可安全暴露给控制面的稳定 OAuth 错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthErrorCode {
    /// 用户拒绝授权。
    AccessDenied,
    /// code 或 refresh token 无效或已被消费。
    InvalidGrant,
    /// 配置的 public client 被拒绝。
    InvalidClient,
    /// 请求的官方 scope 集合被拒绝。
    InvalidScope,
    /// 授权服务器暂时不可用。
    TemporarilyUnavailable,
    /// 无法识别的错误码；不保留服务端原文。
    Other,
}

/// 失败后协调方可采取的处置类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// 瞬时失败，允许后续独立协调的重试。
    Transient,
    /// 发送状态不确定，不应在同一 exchange 内重放一次性凭据。
    Ambiguous,
    /// 当前 revision 的凭据被永久拒绝。
    CredentialPermanent,
    /// Provider 或 client 配置被永久拒绝。
    ConfigurationPermanent,
    /// 用户已拒绝，或需要重新发起交互流程。
    UserActionRequired,
    /// 协议或信任边界校验失败并 fail closed。
    Security,
    /// 官方部署不提供该流程。
    Unsupported,
}

/// 发起网络请求前检出的配置错误。
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// 回调 URI 格式非法、不安全或包含禁止成分。
    #[error("invalid OAuth redirect URI")]
    InvalidRedirectUri,
    /// 回调 URI 未登记在本地 allowlist。
    #[error("OAuth redirect URI is not allowlisted")]
    RedirectUriNotAllowlisted,
    /// issuer 不是官方 Grok Build issuer。
    #[error("OIDC issuer is not the official Grok Build issuer")]
    UntrustedIssuer,
    /// 发现文档中的端点离开了官方 issuer origin。
    #[error("OIDC discovery returned an untrusted endpoint")]
    UntrustedEndpoint,
    /// 可选的 team principal 元数据为空或含不安全文本。
    #[error("invalid OAuth principal metadata")]
    InvalidPrincipal,
}

/// 按字段定位的脱敏协议校验错误。
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolViolation {
    /// JSON 无法解析为预期 wire 结构。
    #[error("OAuth response is not valid JSON")]
    InvalidJson,
    /// 缺少必需的 wire 字段。
    #[error("OAuth response is missing field `{0}`")]
    MissingField(&'static str),
    /// wire 字段违反格式或长度约束。
    #[error("OAuth response contains invalid field `{0}`")]
    InvalidField(&'static str),
    /// 响应超出解析器的体积上限。
    #[error("OAuth response exceeds the maximum body size")]
    ResponseTooLarge,
    /// 发现文档允许不安全的 `none` 签名算法。
    #[error("OIDC discovery advertises an insecure signing algorithm")]
    InsecureSigningAlgorithm,
}

/// 回调失败原因；不保留 code 与 state 值。
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CallbackRejection {
    /// query 中出现重复的安全敏感参数。
    #[error("OAuth callback contains a duplicate parameter")]
    DuplicateParameter,
    /// 回调缺少一次性 state。
    #[error("OAuth callback is missing state")]
    MissingState,
    /// 回调缺少 authorization code。
    #[error("OAuth callback is missing code")]
    MissingCode,
    /// 一次性 state 与 pending flow 不匹配。
    #[error("OAuth callback state mismatch")]
    StateMismatch,
    /// 授权服务器返回 `access_denied`。
    #[error("OAuth authorization was denied")]
    AccessDenied,
    /// 授权服务器返回其他回调错误。
    #[error("OAuth authorization callback was rejected")]
    ProviderRejected,
}

/// 未验证 token 集合无法通过凭据边界的原因。
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum VerificationFailure {
    /// 未注入 JWT/JWKS 或权威 user-info 校验器。
    #[error("token verification is unavailable; refusing unverified credentials")]
    Unavailable,
    /// Authorization Code 响应缺少必需的 ID token。
    #[error("authorization-code response is missing an ID token")]
    MissingIdToken,
    /// 校验器拒绝了签名、issuer、audience、nonce、过期时间或身份。
    #[error("token verification failed")]
    Rejected,
    /// Authorization Code 流程未经 ID token 验证。
    #[error("authorization-code flow requires verified ID-token evidence")]
    WrongEvidence,
}

/// 不含密钥的 OAuth 协议错误。
#[derive(Debug, Error)]
pub enum OAuthError {
    /// 本地信任或回调配置非法。
    #[error(transparent)]
    Configuration(#[from] ConfigError),
    /// 可替换 transport 失败；不保留原始错误信息。
    #[error("OAuth transport failed during {operation:?}: {kind:?}")]
    Transport {
        /// 执行中的操作。
        operation: OAuthOperation,
        /// 脱敏的发送状态分类。
        kind: TransportFailureKind,
    },
    /// 非成功 HTTP 响应，且没有可识别的 OAuth 错误码。
    #[error("OAuth endpoint returned HTTP {status} during {operation:?}")]
    HttpStatus {
        /// 执行中的操作。
        operation: OAuthOperation,
        /// HTTP 状态码。
        status: u16,
    },
    /// 可识别的 OAuth 错误响应。
    #[error("OAuth endpoint returned {code:?} during {operation:?}")]
    Server {
        /// 执行中的操作。
        operation: OAuthOperation,
        /// HTTP 状态码。
        status: u16,
        /// 稳定 OAuth 错误码。
        code: OAuthErrorCode,
    },
    /// token 交换前回调即被拒绝。
    #[error(transparent)]
    Callback(#[from] CallbackRejection),
    /// wire 响应违反严格解析契约。
    #[error("{operation:?} protocol violation: {violation}")]
    Protocol {
        /// 解析中的操作。
        operation: OAuthOperation,
        /// 按字段定位的脱敏原因。
        violation: ProtocolViolation,
    },
    /// token 集合未通过强制验证边界。
    #[error(transparent)]
    Verification(#[from] VerificationFailure),
    /// 流程启动前无法获得安全随机数。
    #[error("secure OAuth entropy is unavailable")]
    EntropyUnavailable,
}

impl OAuthError {
    /// 返回供编排决策使用的低基数失败类别。
    #[must_use]
    pub fn class(&self) -> FailureClass {
        match self {
            Self::Transport {
                kind: TransportFailureKind::Ambiguous | TransportFailureKind::Timeout,
                ..
            } => FailureClass::Ambiguous,
            Self::Transport { .. } => FailureClass::Transient,
            Self::EntropyUnavailable => FailureClass::Transient,
            Self::HttpStatus { status: 429, .. }
            | Self::HttpStatus {
                status: 500..=599, ..
            }
            | Self::Server {
                code: OAuthErrorCode::TemporarilyUnavailable,
                ..
            } => FailureClass::Transient,
            Self::Server {
                code: OAuthErrorCode::InvalidGrant,
                ..
            } => FailureClass::CredentialPermanent,
            Self::Server {
                code: OAuthErrorCode::InvalidClient | OAuthErrorCode::InvalidScope,
                ..
            } => FailureClass::ConfigurationPermanent,
            Self::Server {
                code: OAuthErrorCode::AccessDenied,
                ..
            }
            | Self::Callback(CallbackRejection::AccessDenied) => FailureClass::UserActionRequired,
            Self::Configuration(_)
            | Self::Callback(_)
            | Self::Protocol { .. }
            | Self::Verification(_)
            | Self::Server { .. }
            | Self::HttpStatus { .. } => FailureClass::Security,
        }
    }

    pub(crate) fn transport(operation: OAuthOperation, failure: crate::TransportFailure) -> Self {
        Self::Transport {
            operation,
            kind: failure.kind(),
        }
    }

    pub(crate) fn protocol(operation: OAuthOperation, violation: ProtocolViolation) -> Self {
        Self::Protocol {
            operation,
            violation,
        }
    }
}
