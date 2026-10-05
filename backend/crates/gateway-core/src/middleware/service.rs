//! 公开服务的跨进程组合边界；原生调用仍使用所属领域的输入输出类型

use crate::{
    engine::{extensions::ExtensionCallScope, middleware::FrozenMiddlewarePlan},
    lifecycle::CancellationToken,
};

/// 值只在进入插件链时编码；操作的领域校验与副作用由终端所属服务执行
pub type Value = serde_json::Value;
pub type Next = super::Next<Value, Value, Error>;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl Error {
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: "invalid".into(),
            message: message.into(),
            details: None,
        }
    }

    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: "unavailable".into(),
            message: message.into(),
            details: None,
        }
    }
}

/// 子服务继承同一发布计划及取消信号；防递归集合不承担字段或权限过滤
#[derive(Clone, Debug)]
pub struct Context {
    pub operation: &'static str,
    pub request_id: String,
    pub call_id: String,
    pub parent_call_id: Option<String>,
    pub cancellation: CancellationToken,
    pub extensions: ExtensionCallScope,
    pub plan: FrozenMiddlewarePlan,
}
