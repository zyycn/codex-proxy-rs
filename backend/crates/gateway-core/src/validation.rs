//! 领域值对象的纯校验错误与文本约束，不依赖执行事件或路由状态。

use thiserror::Error;

/// 应用层标识不满足约束。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdentifierError {
    /// 标识为空。
    #[error("identifier must not be empty")]
    Empty,
    /// 标识超过核心允许的字节数。
    #[error("identifier exceeds {max_bytes} bytes")]
    TooLong {
        /// 最大字节数。
        max_bytes: usize,
    },
    /// 标识使用了保留的系统前缀。
    #[error("identifier uses the reserved system prefix")]
    ReservedPrefix,
    /// 标识包含控制字符。
    #[error("identifier contains control characters")]
    ControlCharacter,
    /// 标识缺少规定的语义前缀。
    #[error("identifier must start with `{expected}`")]
    MissingPrefix {
        /// 规定前缀。
        expected: &'static str,
    },
    /// 标识不满足该领域值对象的固定格式。
    #[error("identifier has an invalid format")]
    InvalidFormat,
}

/// Operation 构造或校验失败。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OperationError {
    /// 必填文本为空。
    #[error("`{field}` must not be empty")]
    EmptyField {
        /// 字段名。
        field: &'static str,
    },
    /// 数量字段为零。
    #[error("`{field}` must be greater than zero")]
    ZeroValue {
        /// 字段名。
        field: &'static str,
    },
    /// JSON 字段必须是 object。
    #[error("`{field}` must be a JSON object")]
    JsonObjectRequired {
        /// 字段名。
        field: &'static str,
    },
}

/// 路由快照或 Route Plan 不满足不变量。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RoutingError {
    /// 动态 Provider/model 标识无法构造。
    #[error("routing identifier is invalid")]
    InvalidIdentifier,
    /// 配置 revision 必须为正数。
    #[error("config revision must be greater than zero")]
    InvalidRevision,
    /// Restricted scope 必须保留至少一个持久化分组 binding。
    #[error("account routing scope is invalid")]
    InvalidAccountScope,
    /// Key 的账号范围中当前没有任何可参与路由的账号。
    #[error("account routing scope is empty")]
    EmptyAccountScope,
    /// 快照中存在重复实体。
    #[error("duplicate {entity} `{id}`")]
    DuplicateEntity {
        /// 实体类型。
        entity: &'static str,
        /// 实体 ID。
        id: String,
    },
    /// 实体引用不存在。
    #[error("{entity} `{id}` was not found")]
    NotFound {
        /// 实体类型。
        entity: &'static str,
        /// 实体 ID。
        id: String,
    },
    /// 固定平台内没有可执行本次请求的 Provider。
    #[error("no provider can execute model `{model}`")]
    NoCapableProvider {
        /// 客户端提交的模型名称。
        model: String,
    },
    /// 固定 Provider 的原生端点当前不可执行。
    #[error("provider endpoint `{provider}` is unavailable")]
    NoCapableProviderEndpoint {
        /// adapter 已绑定的 Provider。
        provider: String,
    },
}

/// 调用方策略不满足约束或拒绝请求。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PolicyError {
    /// 请求超过调用方策略。
    #[error("request was denied by caller policy: {reason}")]
    Denied {
        /// 稳定拒绝原因。
        reason: &'static str,
    },
}

/// 用量或价格估算不满足事实约束。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MeteringError {
    /// 十进制定点值格式无效或超过 `numeric(20, 10)`。
    #[error("decimal value must fit unsigned numeric(20, 10)")]
    InvalidDecimal,
    /// 货币代码无效。
    #[error("currency must be a three-letter uppercase ASCII code")]
    InvalidCurrency,
}

pub(crate) fn validate_text(
    value: &str,
    max_bytes: usize,
    reject_reserved_prefix: bool,
    required_prefix: Option<&'static str>,
) -> Result<(), IdentifierError> {
    if value.is_empty() {
        return Err(IdentifierError::Empty);
    }
    if value.len() > max_bytes {
        return Err(IdentifierError::TooLong { max_bytes });
    }
    if value.chars().any(char::is_control) {
        return Err(IdentifierError::ControlCharacter);
    }
    if reject_reserved_prefix && value.starts_with("__") {
        return Err(IdentifierError::ReservedPrefix);
    }
    if let Some(prefix) = required_prefix
        && !value.starts_with(prefix)
    {
        return Err(IdentifierError::MissingPrefix { expected: prefix });
    }
    Ok(())
}
