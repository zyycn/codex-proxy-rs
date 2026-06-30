use codex_proxy_rs::infra::format::{format_compact_number, format_duration_ms};

#[test]
fn format_compact_number_should_scale_with_suffixes() {
    assert_eq!(format_compact_number(999), "999");
    assert_eq!(format_compact_number(2_860), "2.86K");
    assert_eq!(format_compact_number(920_400), "920.4K");
    assert_eq!(format_compact_number(5_904_106), "5.9M");
    assert_eq!(format_compact_number(1_200_000_000), "1.2B");
    assert_eq!(format_compact_number(3_400_000_000_000), "3.4T");
    assert_eq!(format_compact_number(5_600_000_000_000_000), "5.6P");
}

#[test]
fn format_duration_ms_should_scale_units() {
    assert_eq!(format_duration_ms(Some(850)), "850 ms");
    assert_eq!(format_duration_ms(Some(1_250)), "1.25 s");
    assert_eq!(format_duration_ms(Some(12_500)), "12.5 s");
    assert_eq!(format_duration_ms(Some(90_000)), "1.5 min");
    assert_eq!(format_duration_ms(Some(7_200_000)), "2.0 h");
    assert_eq!(format_duration_ms(Some(172_800_000)), "2.0 d");
}
