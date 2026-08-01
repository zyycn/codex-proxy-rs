//! 备份调度与保留策略（backup/policy.rs）的纯逻辑测试。

use chrono::{Duration, TimeZone as _, Utc};

use gateway_admin::{
    backup::policy::{BackupSchedule, RetentionReason, decide_retention},
    model::backup::{BackupRecord, BackupStatus, BackupTriggerKind},
};

fn record(
    id: &str,
    trigger: BackupTriggerKind,
    status: BackupStatus,
    created_at: chrono::DateTime<Utc>,
    completed_at: Option<chrono::DateTime<Utc>>,
) -> BackupRecord {
    BackupRecord {
        id: id.to_owned(),
        trigger_kind: trigger,
        status,
        scheduled_at: if trigger == BackupTriggerKind::Scheduled {
            Some(created_at)
        } else {
            None
        },
        object_key: format!("k/{id}"),
        size_bytes: Some(1),
        sha256: Some("a".repeat(64)),
        attempt_count: 0,
        error_code: None,
        error_message: None,
        started_at: Some(created_at),
        completed_at,
        expires_at: None,
        created_at,
        updated_at: created_at,
    }
}

#[test]
fn parse_rejects_bad_cron_and_timezone() {
    assert!(BackupSchedule::parse("61 * * * *", "Asia/Shanghai").is_err());
    assert!(BackupSchedule::parse("0 2 * * *", "Not/AZone").is_err());
    assert!(BackupSchedule::parse("0 2 * * *", "Asia/Shanghai").is_ok());
}

#[test]
fn next_after_is_strictly_later() {
    let schedule = BackupSchedule::parse("0 2 * * *", "Asia/Shanghai").unwrap();
    let at = Utc.with_ymd_and_hms(2026, 8, 1, 18, 0, 0).unwrap();
    let next = schedule.next_after(at).unwrap();
    assert_eq!(next, Utc.with_ymd_and_hms(2026, 8, 2, 18, 0, 0).unwrap());
}

#[test]
fn last_firing_is_at_or_before() {
    let schedule = BackupSchedule::parse("0 2 * * *", "Asia/Shanghai").unwrap();
    // 北京时间 02:00 = 前一日 18:00 UTC。
    let before = Utc.with_ymd_and_hms(2026, 8, 1, 17, 59, 0).unwrap();
    assert_eq!(
        schedule.last_firing_at_or_before(before).unwrap(),
        Utc.with_ymd_and_hms(2026, 7, 31, 18, 0, 0).unwrap()
    );
    let exact = Utc.with_ymd_and_hms(2026, 8, 1, 18, 0, 0).unwrap();
    assert_eq!(schedule.last_firing_at_or_before(exact).unwrap(), exact);
    let after = Utc.with_ymd_and_hms(2026, 8, 1, 19, 0, 0).unwrap();
    assert_eq!(schedule.last_firing_at_or_before(after).unwrap(), exact);
}

#[test]
fn dst_spring_forward_skips_missing_hour() {
    let schedule = BackupSchedule::parse("30 2 * * *", "America/New_York").unwrap();
    let at = Utc.with_ymd_and_hms(2026, 3, 8, 6, 29, 0).unwrap(); // 01:29 EST
    let next = schedule.next_after(at).unwrap();
    assert_eq!(next, Utc.with_ymd_and_hms(2026, 3, 9, 6, 30, 0).unwrap());
    let at_after_spring = Utc.with_ymd_and_hms(2026, 3, 8, 10, 0, 0).unwrap(); // 06:00 EDT
    let last = schedule.last_firing_at_or_before(at_after_spring).unwrap();
    assert_eq!(last, Utc.with_ymd_and_hms(2026, 3, 7, 7, 30, 0).unwrap());
}

#[test]
fn retention_count_keeps_newest_n() {
    let now = Utc::now();
    let base = now - Duration::days(10);
    let records = vec![
        record(
            "newest",
            BackupTriggerKind::Scheduled,
            BackupStatus::Completed,
            base + Duration::days(2),
            Some(base + Duration::days(2)),
        ),
        record(
            "middle",
            BackupTriggerKind::Scheduled,
            BackupStatus::Completed,
            base + Duration::days(1),
            Some(base + Duration::days(1)),
        ),
        record(
            "oldest",
            BackupTriggerKind::Scheduled,
            BackupStatus::Completed,
            base,
            Some(base),
        ),
    ];
    let decisions = decide_retention(0, 2, now, &records);
    let ids: Vec<_> = decisions.iter().map(|d| d.record_id.as_str()).collect();
    assert_eq!(ids, vec!["oldest"]);
    assert_eq!(decisions[0].reason, RetentionReason::Count);
}

#[test]
fn retention_days_deletes_old_scheduled_but_keeps_newest() {
    let now = Utc::now();
    let records = vec![
        record(
            "recent",
            BackupTriggerKind::Scheduled,
            BackupStatus::Completed,
            now - Duration::days(1),
            Some(now - Duration::days(1)),
        ),
        record(
            "ancient",
            BackupTriggerKind::Scheduled,
            BackupStatus::Completed,
            now - Duration::days(40),
            Some(now - Duration::days(40)),
        ),
    ];
    let decisions = decide_retention(30, 0, now, &records);
    let ids: Vec<_> = decisions.iter().map(|d| d.record_id.as_str()).collect();
    assert_eq!(ids, vec!["ancient"]);
}

#[test]
fn newest_scheduled_never_auto_deleted_even_when_ancient() {
    let now = Utc::now();
    let records = vec![record(
        "sole",
        BackupTriggerKind::Scheduled,
        BackupStatus::Completed,
        now - Duration::days(400),
        Some(now - Duration::days(400)),
    )];
    assert!(decide_retention(30, 1, now, &records).is_empty());
}

#[test]
fn disabled_thresholds_delete_nothing() {
    let now = Utc::now();
    let records = vec![
        record(
            "newest",
            BackupTriggerKind::Scheduled,
            BackupStatus::Completed,
            now - Duration::days(2),
            Some(now - Duration::days(2)),
        ),
        record(
            "old",
            BackupTriggerKind::Scheduled,
            BackupStatus::Completed,
            now - Duration::days(90),
            Some(now - Duration::days(90)),
        ),
    ];
    assert!(decide_retention(0, 0, now, &records).is_empty());
}
