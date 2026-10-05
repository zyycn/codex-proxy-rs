//! 账号分组的 Fast 三态策略与多分组优先级

use serde::{Deserialize, Serialize};

/// 分组冻结的 Fast 策略；默认保留客户端选择
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FastMode {
    #[default]
    Default,
    Enabled,
    Disabled,
}

impl FastMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "enabled" => Some(Self::Enabled),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }

    /// 多分组冲突时关闭优先于开启，默认不覆盖其他分组
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Disabled, _) | (_, Self::Disabled) => Self::Disabled,
            (Self::Enabled, _) | (_, Self::Enabled) => Self::Enabled,
            _ => Self::Default,
        }
    }
}
