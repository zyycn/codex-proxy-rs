//! 用量计数使用十进制字符串，避免插件前端丢失整数精度

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
use std::fmt;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsageCount(pub u64);
impl UsageCount {
    pub const ZERO: Self = Self(0);
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}
impl From<u64> for UsageCount {
    fn from(value: u64) -> Self {
        Self(value)
    }
}
impl fmt::Display for UsageCount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
impl Serialize for UsageCount {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for UsageCount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(D::Error::custom("count must be an unsigned decimal string"));
        }
        value.parse().map(Self).map_err(D::Error::custom)
    }
}

/// 有符号统计贡献受 PostgreSQL bigint 范围限制，JSON 使用十进制字符串
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsageDelta(pub i64);
impl UsageDelta {
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}
impl Serialize for UsageDelta {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for UsageDelta {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let digits = value.strip_prefix('-').unwrap_or(&value);
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(D::Error::custom("delta must be a signed decimal string"));
        }
        value.parse().map(Self).map_err(D::Error::custom)
    }
}
