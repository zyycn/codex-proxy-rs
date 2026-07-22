//! Codex transport 私有时间辅助。

use chrono::{DateTime, FixedOffset, Utc};

const CHINA_OFFSET_SECONDS: i32 = 8 * 60 * 60;

pub(crate) fn china_filename_timestamp_millis(value: &DateTime<Utc>) -> String {
    value
        .with_timezone(&china_offset())
        .format("%Y%m%dT%H%M%S%.3f%z")
        .to_string()
}

fn china_offset() -> FixedOffset {
    FixedOffset::east_opt(CHINA_OFFSET_SECONDS).expect("valid China timezone offset")
}
