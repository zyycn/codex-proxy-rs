use chrono::{DateTime, Utc};
use codex_proxy_rs::infra::time::{
    china_datetime, china_day_start, china_filename_timestamp_millis, china_hour, china_hour_start,
    china_quarter_hour_start, china_rfc3339_millis,
};

#[test]
fn china_day_start_should_use_china_calendar_day() {
    let value = "2026-06-24T18:30:00Z".parse::<DateTime<Utc>>().unwrap();

    assert_eq!(
        china_day_start(value).to_rfc3339(),
        "2026-06-24T16:00:00+00:00"
    );
}

#[test]
fn china_rfc3339_millis_should_keep_china_offset() {
    let value = "2026-06-24T16:36:59.190910486Z"
        .parse::<DateTime<Utc>>()
        .unwrap();

    assert_eq!(
        china_rfc3339_millis(&value),
        "2026-06-25T00:36:59.190+08:00"
    );
}

#[test]
fn china_datetime_should_include_complete_china_calendar_time() {
    let value = "2026-07-13T16:10:06.969Z".parse::<DateTime<Utc>>().unwrap();

    assert_eq!(china_datetime(&value), "2026-07-14 00:10:06");
}

#[test]
fn china_filename_timestamp_millis_should_keep_china_offset_without_colons() {
    let value = "2026-06-24T16:36:59.190910486Z"
        .parse::<DateTime<Utc>>()
        .unwrap();

    assert_eq!(
        china_filename_timestamp_millis(&value),
        "20260625T003659.190+0800"
    );
}

#[test]
fn china_hour_start_should_use_china_hour() {
    let value = "2026-06-24T18:30:00Z".parse::<DateTime<Utc>>().unwrap();

    assert_eq!(
        china_hour_start(value).to_rfc3339(),
        "2026-06-24T18:00:00+00:00"
    );
    assert_eq!(china_hour(&value), 2);
}

#[test]
fn china_quarter_hour_start_should_use_china_quarter_hour() {
    let value = "2026-06-24T18:37:00Z".parse::<DateTime<Utc>>().unwrap();

    assert_eq!(
        china_quarter_hour_start(value).to_rfc3339(),
        "2026-06-24T18:30:00+00:00"
    );
}
