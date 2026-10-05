//! usage 观测抖动映射的确定性边界。

use std::time::Duration;

use provider_openai::{catalog_refresh_jitter, quota_failure_refresh_delay, uniform_delay};

#[test]
fn uniform_delay_maps_samples_into_half_open_range() {
    let max = Duration::from_secs(600);
    assert_eq!(
        uniform_delay(None, max),
        Duration::ZERO,
        "随机源退化时不延迟"
    );
    assert_eq!(uniform_delay(Some(0), max), Duration::ZERO);
    assert_eq!(
        uniform_delay(Some(u64::MAX), Duration::ZERO),
        Duration::ZERO,
        "零上限不产生延迟"
    );
    // 样本按 [0, max) 取模映射，边界样本必须落在区间内。
    for sample in [1, 1_000, u64::MAX / 3, u64::MAX / 2, u64::MAX - 1, u64::MAX] {
        let delay = uniform_delay(Some(sample), max);
        assert!(delay < max, "sample {sample} 超出上限: {delay:?}");
    }
    assert_eq!(
        uniform_delay(Some(u64::MAX), max).as_nanos(),
        u128::from(u64::MAX) % max.as_nanos()
    );
    // 固定样本集合应产生分散的延迟，覆盖区间的不同位置。
    let delays: Vec<_> = (0..8_u64)
        .map(|index| uniform_delay(Some(index * (u64::MAX / 8)), max))
        .collect();
    assert!(
        delays.iter().any(|delay| *delay < max / 2),
        "样本集合应覆盖区间下半段"
    );
    assert!(
        delays.iter().any(|delay| *delay >= max / 2),
        "样本集合应覆盖区间上半段"
    );
}

#[test]
fn quota_failure_refresh_delay_stays_within_settlement_window() {
    assert_eq!(
        quota_failure_refresh_delay(None),
        Duration::from_secs(2),
        "随机源退化时回到固定 2s 基准"
    );
    let delays: Vec<_> = [1, 1_000, u64::MAX / 3, u64::MAX / 2, u64::MAX - 1, u64::MAX]
        .into_iter()
        .map(|sample| (sample, quota_failure_refresh_delay(Some(sample))))
        .collect();
    for (sample, delay) in &delays {
        assert!(delay >= &Duration::from_secs(1), "sample {sample} 低于 1s");
        assert!(delay < &Duration::from_secs(3), "sample {sample} 达到 3s");
    }
    assert_eq!(
        quota_failure_refresh_delay(Some(0)),
        Duration::from_secs(1),
        "零样本映射到下界，保持确定性"
    );
    // 跨度若误用「基准 − 下限」，该样本只会映射到 1.5s；
    // 按 [1s, 3s) 全区间均匀映射应落在上半区间的 2.5s。
    assert_eq!(
        quota_failure_refresh_delay(Some(1_500_000_000)),
        Duration::from_millis(2_500),
        "样本 1_500_000_000 应映射到 2.5s"
    );
    // 固定样本集合需同时覆盖上下半区间，防止区间被静默收窄。
    assert!(
        delays
            .iter()
            .any(|(_, delay)| *delay < Duration::from_secs(2)),
        "样本集合应覆盖区间下半段 [1s, 2s)"
    );
    assert!(
        delays
            .iter()
            .any(|(_, delay)| *delay >= Duration::from_secs(2)),
        "样本集合应覆盖区间上半段 [2s, 3s)"
    );
}

#[test]
fn catalog_refresh_jitter_is_single_sided_and_bounded() {
    let interval = Duration::from_secs(15 * 60);
    assert_eq!(
        catalog_refresh_jitter(None, interval),
        Duration::ZERO,
        "随机源退化时不抖动"
    );
    assert_eq!(catalog_refresh_jitter(Some(0), interval), Duration::ZERO);
    assert_eq!(
        catalog_refresh_jitter(Some(u64::MAX), Duration::ZERO),
        Duration::ZERO,
        "周期过小时不抖动"
    );
    for sample in [1, 1_000, u64::MAX / 3, u64::MAX / 2, u64::MAX - 1, u64::MAX] {
        let jitter = catalog_refresh_jitter(Some(sample), interval);
        assert!(
            jitter < interval / 5,
            "sample {sample} 抖动超过周期的 20%: {jitter:?}"
        );
    }
    assert_eq!(
        catalog_refresh_jitter(Some(u64::MAX), interval).as_nanos(),
        u128::from(u64::MAX) % (interval.as_nanos() / 5)
    );
}
