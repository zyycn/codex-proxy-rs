//! 时间格式化辅助。

use chrono::{DateTime, FixedOffset, Utc};
use serde::Serializer;

const CHINA_OFFSET_SECONDS: i32 = 8 * 60 * 60;

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

/// 将 UTC 时间输出为中国时区日期时间。
pub fn china_datetime(value: &DateTime<Utc>) -> String {
    value
        .with_timezone(&china_offset())
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// 将 UTC 时间输出为中国时区日期。
pub fn china_date(value: &DateTime<Utc>) -> String {
    value
        .with_timezone(&china_offset())
        .format("%Y-%m-%d")
        .to_string()
}

/// 将 UTC 时间输出为中国时区时间。
pub fn china_time(value: &DateTime<Utc>) -> String {
    value
        .with_timezone(&china_offset())
        .format("%H:%M:%S")
        .to_string()
}

/// 将 UTC 时间输出为相对时间，超过 7 天时返回中国时区日期。
pub fn china_relative_time(value: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(value) = value else {
        return "-".to_string();
    };
    let diff = now.signed_duration_since(value);
    let minutes = diff.num_minutes();
    let hours = diff.num_hours();
    let days = diff.num_days();

    if minutes < 1 {
        "刚刚".to_string()
    } else if minutes < 60 {
        format!("{minutes}分钟前")
    } else if hours < 24 {
        format!("{hours}小时前")
    } else if days < 7 {
        format!("{days}天前")
    } else {
        china_date(&value)
    }
}

/// 将 RFC3339 字符串输出为相对时间，超过 7 天时返回中国时区日期。
pub fn china_relative_time_str(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "-".to_string();
    };
    DateTime::parse_from_rfc3339(value)
        .map(|datetime| china_relative_time(Some(datetime.with_timezone(&Utc)), Utc::now()))
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
