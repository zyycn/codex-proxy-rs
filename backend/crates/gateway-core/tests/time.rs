//! 验证部署时区的日历边界、夏令时歧义与无效时间处理

use chrono::{DateTime, NaiveDate, Utc};
use gateway_core::time::DeploymentTimeZone;

fn utc(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

#[test]
fn calendar_boundaries_follow_offsets_and_dst_instead_of_fixed_hours() {
    for (zone, at, start, end, hours) in [
        (
            "Asia/Shanghai",
            "2026-01-01T00:00:00Z",
            "2025-12-31T16:00:00Z",
            "2026-01-01T16:00:00Z",
            24,
        ),
        (
            "UTC",
            "2026-01-01T05:00:00Z",
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            24,
        ),
        (
            "Asia/Kathmandu",
            "2026-01-01T00:00:00Z",
            "2025-12-31T18:15:00Z",
            "2026-01-01T18:15:00Z",
            24,
        ),
        (
            "America/New_York",
            "2026-03-08T16:00:00Z",
            "2026-03-08T05:00:00Z",
            "2026-03-09T04:00:00Z",
            23,
        ),
        (
            "America/New_York",
            "2026-11-01T16:00:00Z",
            "2026-11-01T04:00:00Z",
            "2026-11-02T05:00:00Z",
            25,
        ),
    ] {
        let zone: DeploymentTimeZone = zone.parse().unwrap();
        let at = utc(at);
        assert_eq!(zone.day_start(at), Some(utc(start)));
        assert_eq!(zone.days_after(at, 1), Some(utc(end)));
        assert_eq!((utc(end) - utc(start)).num_hours(), hours);
    }
    let zone: DeploymentTimeZone = "America/New_York".parse().unwrap();
    let start = utc("2026-03-06T05:00:00Z");
    assert_eq!(zone.days_after(start, 7), Some(utc("2026-03-13T04:00:00Z")));
}

#[test]
fn local_slots_skip_gaps_and_pick_only_the_first_fold_instant() {
    let zone: DeploymentTimeZone = "America/New_York".parse().unwrap();
    assert!(
        zone.resolve_local("2026-03-08T02:30:00".parse().unwrap())
            .is_none()
    );
    assert_eq!(
        zone.resolve_local("2026-11-01T01:30:00".parse().unwrap()),
        Some(utc("2026-11-01T05:30:00Z"))
    );
    let zone: DeploymentTimeZone = "America/Sao_Paulo".parse().unwrap();
    assert_eq!(
        zone.date_start(NaiveDate::from_ymd_opt(2018, 11, 4).unwrap()),
        Some(utc("2018-11-04T03:00:00Z"))
    );
    let zone: DeploymentTimeZone = "Pacific/Apia".parse().unwrap();
    assert_eq!(
        zone.date_start(NaiveDate::from_ymd_opt(2011, 12, 30).unwrap()),
        Some(utc("2011-12-30T10:00:00Z"))
    );
}

#[test]
fn deployment_timezone_rejects_invalid_names_and_types() {
    assert_eq!(DeploymentTimeZone::default().name(), "Asia/Shanghai");
    for value in [
        serde_json::json!(""),
        serde_json::json!("UTC+8"),
        serde_json::json!("Not/AZone"),
        serde_json::json!(null),
        serde_json::json!(8),
    ] {
        assert!(serde_json::from_value::<DeploymentTimeZone>(value).is_err());
    }
}

#[test]
fn calendar_boundaries_reject_unrepresentable_local_dates() {
    for (zone, value) in [
        ("Asia/Shanghai", DateTime::<Utc>::MAX_UTC),
        ("America/New_York", DateTime::<Utc>::MIN_UTC),
    ] {
        let zone: DeploymentTimeZone = zone.parse().unwrap();
        assert_eq!(zone.day_start(value), None);
        assert_eq!(zone.days_before(value, 0), None);
        assert_eq!(zone.days_after(value, 0), None);
    }
}
