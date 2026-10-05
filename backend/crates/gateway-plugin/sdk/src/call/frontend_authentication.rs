//! 数据面入口认证合同；宿主拥有 principal 到执行身份的映射

use std::fmt;

use serde::{Deserialize, Serialize};

/// 插件在注册阶段返回的稳定认证器标识
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontendAuthenticationIdentifier {
    pub identifier: String,
}

/// API 提取的有界认证信封；Cookie、管理会话及客户端画像不进入插件调用
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontendAuthenticationRequest {
    pub authorization: String,
}

impl fmt::Debug for FrontendAuthenticationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrontendAuthenticationRequest")
            .field("authorization", &"[REDACTED]")
            .finish()
    }
}

/// 插件只声明外部 principal，不得选择宿主 Client Key
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum FrontendAuthenticationResult {
    Authenticated { principal: String },
    NotMatched {},
    Rejected {},
}

impl fmt::Debug for FrontendAuthenticationResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authenticated { .. } => formatter
                .debug_struct("Authenticated")
                .field("principal", &"[REDACTED]")
                .finish(),
            Self::NotMatched {} => formatter.write_str("NotMatched"),
            Self::Rejected {} => formatter.write_str("Rejected"),
        }
    }
}
