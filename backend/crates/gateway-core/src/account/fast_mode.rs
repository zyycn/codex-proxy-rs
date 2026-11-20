//! 账号分组的快速模式策略与多分组优先级

use serde::{Deserialize, Serialize};

/// 分组冻结的 Fast 策略；默认保留客户端选择
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FastMode {
    #[default]
    Default,
    Enabled,
    Disabled,
    Ultrafast,
}

impl FastMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Enabled => "enabled",
            Self::Ultrafast => "ultrafast",
            Self::Disabled => "disabled",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "enabled" => Some(Self::Enabled),
            "ultrafast" => Some(Self::Ultrafast),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }

    /// 多分组冲突时禁用优先；同时强制两种加速档位时选用较低的 Fast
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Disabled, _) | (_, Self::Disabled) => Self::Disabled,
            (Self::Enabled, _) | (_, Self::Enabled) => Self::Enabled,
            (Self::Ultrafast, _) | (_, Self::Ultrafast) => Self::Ultrafast,
            _ => Self::Default,
        }
    }
}
