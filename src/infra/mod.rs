#![deny(missing_docs)]

//! 平台基础设施层，承载加密、身份、存储、日志和 JSON 原语。

use chrono::{DateTime, FixedOffset, Utc};
use serde::Serializer;

const CHINA_OFFSET_SECONDS: i32 = 8 * 60 * 60;

/// 加密原语。
pub mod crypto;
/// 数据库连接与 SQLite 管理。
pub mod database;
/// 身份验证原语（管理员密码、API Key 哈希）。
pub mod identity;
/// JSON 序列化和分页原语。
pub mod json;
/// 日志初始化和轮换。
pub mod logging;
/// 路径和安装 ID 辅助。
pub mod paths;

/// 将 UTC 时间输出为中国时区 RFC3339 字符串。
pub fn china_rfc3339(value: &DateTime<Utc>) -> String {
    value.with_timezone(&china_offset()).to_rfc3339()
}

/// 将 RFC3339 字符串输出为中国时区 RFC3339 字符串。
pub fn china_rfc3339_str(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|datetime| datetime.with_timezone(&china_offset()).to_rfc3339())
        .unwrap_or_else(|_| value.to_string())
}

/// 将 RFC3339 字符串输出为中国时区日期时间。
pub fn china_datetime_rfc3339_str(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|datetime| {
            datetime
                .with_timezone(&china_offset())
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| value.to_string())
}

/// Serde 序列化 UTC 时间为中国时区 RFC3339 字符串。
pub fn serialize_china_rfc3339<S>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&china_rfc3339(value))
}

fn china_offset() -> FixedOffset {
    FixedOffset::east_opt(CHINA_OFFSET_SECONDS).expect("valid China timezone offset")
}
