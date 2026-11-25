//! 对应生产职责的行为测试

#[test]
fn warmup_cron_validation_and_calendar_matching() {
    use chrono::NaiveDate;
    use gateway_core::provider_ports::{ProviderWarmupPolicy, valid_warmup_cron};

    for value in [
        "0 8 * * *",
        "*/15 8-18 * * MON-FRI",
        "0 8,13 * * MON-FRI",
        "0 0 29 FEB *",
    ] {
        assert!(valid_warmup_cron(value), "{value}");
    }
    for value in [
        "",
        "08:00",
        "08:00,13:00",
        "60 8 * * *",
        "0 24 * * *",
        "0 8 * *",
        "0 0 8 * * *",
        "0 8 * * * *",
        "@daily",
        "0 8 * * *\n30 13 * * *",
        "0 8 * * *\r\n30 13 * * *",
        "0 8 * * *\t",
        " 0 8 * * *",
        "0 8 * * *\n",
    ] {
        assert!(!valid_warmup_cron(value), "{value:?}");
    }
    let policy =
        ProviderWarmupPolicy::try_new(true, "0 8,13 * * MON-FRI", Some("test-model".to_owned()))
            .unwrap();
    assert!(policy.enabled());
    assert_eq!(policy.model(), Some("test-model"));
    for (day, hour, minute, expected) in [
        (9, 8, 0, true),
        (9, 13, 0, true),
        (9, 8, 30, false),
        (9, 13, 30, false),
        (14, 8, 0, false),
        (14, 13, 30, false),
    ] {
        let slot = NaiveDate::from_ymd_opt(2026, 11, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap();
        assert_eq!(policy.matches_slot(slot), expected, "{slot}");
    }
    assert!(ProviderWarmupPolicy::try_new(false, "", None).is_ok());
    assert!(ProviderWarmupPolicy::try_new(true, "", Some("test-model".to_owned())).is_err());
    assert!(ProviderWarmupPolicy::try_new(false, "08:00", None).is_err());
    assert!(!valid_warmup_cron(&format!("0{}8 * * *", " ".repeat(256))));
    let leap = ProviderWarmupPolicy::try_new(false, "0 0 29 FEB *", None).unwrap();
    let slot = NaiveDate::from_ymd_opt(2028, 2, 29)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    assert!(leap.matches_slot(slot));
    assert!(!leap.matches_slot(slot + chrono::Duration::days(1)));
    assert!(ProviderWarmupPolicy::try_new(true, "invalid", None).is_err());
    assert!(ProviderWarmupPolicy::try_new(true, "0 8 * * *", None).is_err());
}

#[test]
fn warmup_matches_only_the_scheduled_hour_and_minute_pairs() {
    use chrono::NaiveDate;
    use gateway_core::provider_ports::ProviderWarmupPolicy;

    let policy = ProviderWarmupPolicy::try_new(
        true,
        "58 5 * * *;59 10 * * *;0 16 * * *;1 21 * * *;0 16 * * MON-FRI",
        Some("test-model".to_owned()),
    )
    .unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
    for minute in 0..1440 {
        let slot = date.and_hms_opt(minute / 60, minute % 60, 0).unwrap();
        assert_eq!(
            policy.matches_slot(slot),
            [358, 659, 960, 1261].contains(&minute),
            "{slot}"
        );
    }
}
