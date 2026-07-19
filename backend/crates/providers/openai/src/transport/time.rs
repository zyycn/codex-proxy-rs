//! Codex transport 私有时间辅助。

use std::time::Instant;

use chrono::{DateTime, FixedOffset, Utc};

const CHINA_OFFSET_SECONDS: i32 = 8 * 60 * 60;

pub(crate) fn elapsed_millis_i64(started_at: Instant) -> i64 {
    started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
}

pub(crate) fn china_filename_timestamp_millis(value: &DateTime<Utc>) -> String {
    value
        .with_timezone(&china_offset())
        .format("%Y%m%dT%H%M%S%.3f%z")
        .to_string()
}

fn china_offset() -> FixedOffset {
    FixedOffset::east_opt(CHINA_OFFSET_SECONDS).expect("valid China timezone offset")
}
