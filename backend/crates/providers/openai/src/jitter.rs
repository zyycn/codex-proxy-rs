//! usage 观测节奏的共享随机源与抖动映射。
//!
//! 多账号池化代理的固定周期请求（整点目录轮询、固定延迟补查、批量首查）
//! 会形成可识别的共享特征；官方 codex 客户端不存在任何周期 usage 轮询。
//! 本模块集中提供观测节奏的随机化策略：周期任务用单侧附加抖动
//! `[0, base + jitter)`（k8s `wait.Jitter`、systemd `RandomizedDelaySec`
//! 同款惯例，不缩短基础周期、不增加请求速率），重试类延迟用小范围均匀
//! 随机替换固定值。所有映射都是纯函数，`None`（系统随机源不可用）时
//! 退化为旧的固定基准，与 `websocket_retry_backoff` 的降级风格一致。

use std::time::Duration;

/// 从系统随机源读取一个 u64 样本；失败返回 `None`，由调用方退化为固定基准。
pub(crate) fn random_u64() -> Option<u64> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes)
        .ok()
        .map(|()| u64::from_le_bytes(bytes))
}

/// 把随机样本映射为 `[0, max)` 内的均匀时长。
///
/// `max` 为零或随机源不可用时返回零，即不引入延迟（保持旧行为）。
#[doc(hidden)]
pub fn uniform_delay(sample: Option<u64>, max: Duration) -> Duration {
    let Some(sample) = sample else {
        return Duration::ZERO;
    };
    if max.is_zero() {
        return Duration::ZERO;
    }
    let nanos = (u128::from(sample) % max.as_nanos()).min(u64::MAX as u128);
    Duration::from_nanos(nanos as u64)
}

/// 额度失败补查的基准延迟；随机源不可用时退回该固定值。
const QUOTA_FAILURE_REFRESH_DELAY: Duration = Duration::from_secs(2);
/// 补查随机区间的下限（含）。
const QUOTA_FAILURE_REFRESH_MIN_DELAY: Duration = Duration::from_secs(1);
/// 补查随机区间的上限（不含），即基准的 +50%。
const QUOTA_FAILURE_REFRESH_MAX_DELAY: Duration = Duration::from_secs(3);

/// 额度拒绝后补查 usage 的随机延迟：基准 2s ±50%，即 `[1s, 3s)`。
///
/// 固定 2 秒会让同时受限的多账号补查完全同步；区间保持在上游结算窗口
/// 附近，避免显著拖慢展示基线。随机源不可用时退回固定基准。
#[doc(hidden)]
pub fn quota_failure_refresh_delay(sample: Option<u64>) -> Duration {
    match sample {
        Some(sample) => {
            // 跨度是「上限 − 下限」而非「基准 − 下限」，否则区间上半段永远取不到。
            QUOTA_FAILURE_REFRESH_MIN_DELAY
                + uniform_delay(
                    Some(sample),
                    QUOTA_FAILURE_REFRESH_MAX_DELAY - QUOTA_FAILURE_REFRESH_MIN_DELAY,
                )
        }
        None => QUOTA_FAILURE_REFRESH_DELAY,
    }
}

/// catalog 周期刷新的单侧抖动比例（20%）。
const CATALOG_REFRESH_JITTER_PER_MILLE: u128 = 200;

/// catalog 周期刷新的附加抖动：`[0, 20%·interval)` 单侧均匀。
///
/// 只追加不缩短：有效周期在配置周期之上随机浮动，打破固定整点节奏，
/// 同时不提高刷新频率。随机源不可用或周期过小时不抖动。
#[doc(hidden)]
pub fn catalog_refresh_jitter(sample: Option<u64>, interval: Duration) -> Duration {
    let Some(sample) = sample else {
        return Duration::ZERO;
    };
    let max_nanos = interval.as_nanos() * CATALOG_REFRESH_JITTER_PER_MILLE / 1_000;
    if max_nanos == 0 {
        return Duration::ZERO;
    }
    let nanos = (u128::from(sample) % max_nanos).min(u64::MAX as u128);
    Duration::from_nanos(nanos as u64)
}
