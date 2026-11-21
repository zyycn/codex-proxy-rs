//! 预测专用的配对采样；不改变完整交付用量或费用账本的统计口径

use chrono::{DateTime, Utc};

use super::provider_credentials::ProviderDocument;

/// 限制带 Provider 文档的历史点数量；累计用量仍覆盖整个查询区间
pub const MAX_FORECAST_HISTORY_POINTS: usize = 128;
const MIN_BLOCK_PERCENT: f64 = 5.0;
const RECENT_BLOCKS: usize = 3;
const MAX_PERCENT_REVERSAL: f64 = 1.0;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuotaForecastUsage {
    pub request_count: u64,
    pub tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub missing_token_count: u64,
    pub known_cost_count: u64,
    pub unavailable_cost_count: u64,
    pub usd: f64,
    pub excluded_request_count: u64,
}

impl QuotaForecastUsage {
    /// 累计计数倒退表示不能配对，不用饱和减法掩盖数据断点
    fn difference(&self, baseline: &Self) -> Option<Self> {
        let usd = self.usd - baseline.usd;
        if !usd.is_finite() || usd < -1e-9 {
            return None;
        }
        Some(Self {
            request_count: self.request_count.checked_sub(baseline.request_count)?,
            tokens: self.tokens.checked_sub(baseline.tokens)?,
            input_tokens: self.input_tokens.checked_sub(baseline.input_tokens)?,
            output_tokens: self.output_tokens.checked_sub(baseline.output_tokens)?,
            cached_input_tokens: self
                .cached_input_tokens
                .checked_sub(baseline.cached_input_tokens)?,
            missing_token_count: self
                .missing_token_count
                .checked_sub(baseline.missing_token_count)?,
            known_cost_count: self
                .known_cost_count
                .checked_sub(baseline.known_cost_count)?,
            unavailable_cost_count: self
                .unavailable_cost_count
                .checked_sub(baseline.unavailable_cost_count)?,
            usd: usd.max(0.0),
            excluded_request_count: self
                .excluded_request_count
                .checked_sub(baseline.excluded_request_count)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct QuotaForecastHistoryPoint {
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub usage: QuotaForecastUsage,
    pub provider_observation: ProviderDocument,
}

#[derive(Debug, Clone, Default)]
pub struct QuotaForecastHistory {
    pub points: Vec<QuotaForecastHistoryPoint>,
    pub usage: QuotaForecastUsage,
    pub pending_request_count: u64,
}

/// 具体 Provider 从自己的历史文档解释出的额度事实
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaForecastObservation {
    pub used_percent: f64,
    pub reset_at: DateTime<Utc>,
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QuotaForecastPoint {
    pub started_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    pub reset_at: DateTime<Utc>,
    pub used_percent: f64,
    pub usage: QuotaForecastUsage,
}

impl QuotaForecastPoint {
    fn follows(&self, previous: &Self) -> bool {
        previous.observed_at <= self.started_at && previous.observed_at < self.observed_at
    }
}

/// 当前窗口内仍有旧周期记录时，只在旧读数已到期、用量回落且新读数得到印证后重设累计
fn cycle_boundary(
    history: &[QuotaForecastPoint],
    current: &QuotaForecastPoint,
) -> (Option<usize>, bool) {
    let mut boundary = None;
    let mut awaiting_confirmation = false;
    for (index, pair) in history.windows(2).enumerate() {
        let (older, previous) = (&pair[0], &pair[1]);
        let first_index = index + 2;
        let first = history.get(first_index).unwrap_or(current);
        let before_reset = older.used_percent.min(previous.used_percent);
        if !previous.follows(older)
            || !first.follows(previous)
            || older.reset_at <= previous.observed_at
            || older.reset_at > first.started_at
            || previous.reset_at > first.started_at
            || before_reset - first.used_percent <= MAX_PERCENT_REVERSAL
        {
            continue;
        }
        // 快照可能复用请求读数；只有另一条在新窗口内独立完成的请求能确认换段
        let confirmation = history
            .get(first_index + 1..)
            .and_then(|later| later.iter().find(|next| next.follows(first)));
        awaiting_confirmation = confirmation.is_none();
        if confirmation.is_some_and(|next| {
            next.observed_at < first.reset_at
                && before_reset - next.used_percent > MAX_PERCENT_REVERSAL
        }) {
            boundary = Some(first_index);
        }
    }
    (boundary, awaiting_confirmation)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaForecastMethod {
    Cumulative,
    Incremental,
}

#[derive(Debug, Clone)]
pub struct QuotaForecastSample {
    pub key: String,
    pub method: QuotaForecastMethod,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub baseline_percent: f64,
    pub sampled_percent: f64,
    pub block_count: usize,
    pub observation_count: usize,
    pub pending_request_count: u64,
    pub discontinuous: bool,
    /// 本周期已记录用量；近期采样仅用于预测剩余量，不能覆盖这份累计值
    pub cycle_usage: QuotaForecastUsage,
    pub usage: QuotaForecastUsage,
}

/// 对同一额度段的累计点形成至少 5 个百分点的连续块，使用最近三个完整块及尾部
/// 不平均逐请求比值；重复百分比不增加块数，5/10 个百分点仅是量化噪声门槛
#[must_use]
pub fn select_forecast_sample(
    key: String,
    start_at: DateTime<Utc>,
    current: QuotaForecastPoint,
    mut history: Vec<QuotaForecastPoint>,
    pending_request_count: u64,
    plan_changed: bool,
) -> QuotaForecastSample {
    history.retain(|point| {
        start_at <= point.observed_at
            && point.observed_at < current.observed_at
            && point.used_percent.is_finite()
            && (0.0..=100.0).contains(&point.used_percent)
    });
    history.sort_by_key(|point| point.observed_at);
    let (boundary, awaiting_confirmation) = cycle_boundary(&history, &current);
    let history = &history[boundary.unwrap_or(0)..];
    let interrupted = plan_changed || boundary.is_some();
    let mut sample = QuotaForecastSample {
        key,
        method: QuotaForecastMethod::Cumulative,
        start_at,
        end_at: current.observed_at,
        baseline_percent: 0.0,
        sampled_percent: current.used_percent,
        block_count: 0,
        observation_count: history.len(),
        pending_request_count,
        discontinuous: interrupted || awaiting_confirmation,
        cycle_usage: current.usage.clone(),
        usage: current.usage.clone(),
    };
    if interrupted {
        let baseline = history.first().unwrap_or(&current);
        let Some(usage) = current.usage.difference(&baseline.usage) else {
            return sample;
        };
        sample.cycle_usage = usage;
    }
    if awaiting_confirmation {
        return sample;
    }
    let mut anchors: Vec<&QuotaForecastPoint> = Vec::new();
    let mut high_water: Option<&QuotaForecastPoint> = None;
    for point in history.iter().chain(std::iter::once(&current)) {
        // 截至快照已过期的读数不能用于预测剩余量，窗口累计仍由上面的周期证据决定
        if point.reset_at <= current.observed_at {
            sample.discontinuous = true;
            continue;
        }
        if let Some(previous) = high_water {
            if point.observed_at <= previous.observed_at {
                continue;
            }
            if point.used_percent < previous.used_percent {
                // 一个百分点内的回落可能是量化/并发乱序；更大的回落不能跨越
                if previous.used_percent - point.used_percent <= MAX_PERCENT_REVERSAL {
                    continue;
                }
                // 同窗口回落不能证明周期重置，只重建采样并保留已记录累计
                sample.discontinuous = true;
                anchors.clear();
            }
        }
        high_water = Some(point);
        if anchors
            .last()
            .is_none_or(|anchor| point.used_percent - anchor.used_percent >= MIN_BLOCK_PERCENT)
        {
            anchors.push(point);
        }
    }
    if anchors.len() < 2 {
        return sample;
    }
    let baseline_index = anchors.len().saturating_sub(RECENT_BLOCKS + 1);
    let baseline = anchors[baseline_index];
    let progress = current.used_percent - baseline.used_percent;
    if !progress.is_finite() || progress < MIN_BLOCK_PERCENT {
        return sample;
    }
    let Some(usage) = current.usage.difference(&baseline.usage) else {
        sample.discontinuous = true;
        return sample;
    };
    if usage.request_count == 0 {
        return sample;
    }
    sample.method = QuotaForecastMethod::Incremental;
    sample.start_at = baseline.observed_at;
    sample.baseline_percent = baseline.used_percent;
    sample.sampled_percent = progress;
    sample.block_count = anchors.len() - baseline_index - 1;
    sample.usage = usage;
    // 断点后的新基线已有足够进度时，只消费新段；旧段从不参与本次估算
    sample.discontinuous = false;
    sample
}
