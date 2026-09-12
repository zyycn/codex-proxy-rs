//! 账号额度的只读容量估算，不参与金额结算或账号调度。

use chrono::{DateTime, Duration, Utc};

use super::provider_credentials::{AccountUsagePeriod, ProviderQuota, ProviderQuotaWindow};
use super::quota_forecast_sampling::{QuotaForecastMethod, QuotaForecastSample};

const DAY_SECONDS: u64 = 86_400;
const MIN_USED_PERCENT: f64 = 5.0;
const LOW_SAMPLE_PERCENT: f64 = 10.0;

#[derive(Debug, Clone, PartialEq)]
pub struct AccountQuotaForecastReport {
    pub account_id: String,
    pub generated_at: DateTime<Utc>,
    pub forecasts: [AccountQuotaForecast; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountQuotaForecast {
    pub period: AccountUsagePeriod,
    pub target_seconds: u64,
    pub extrapolated: bool,
    pub source: Option<QuotaForecastSource>,
    pub unavailable_reason: Option<&'static str>,
    pub low_sample: bool,
    pub incomplete_cost: bool,
    pub incomplete_tokens: bool,
    pub method: QuotaForecastMethod,
    pub estimated_tokens: Option<u64>,
    pub estimated_usd: Option<f64>,
    /// 剩余估算始终属于源窗口，不随目标周期折算。
    pub remaining_tokens: Option<u64>,
    pub remaining_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaForecastSource {
    pub key: String,
    pub label: String,
    pub window_seconds: u64,
    pub used_percent: Option<f64>,
    pub observed_at: Option<DateTime<Utc>>,
    pub start_at: Option<DateTime<Utc>>,
    pub reset_at: DateTime<Utc>,
    pub request_count: u64,
    pub tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub known_cost_count: u64,
    pub partial_cost_count: u64,
    pub unavailable_cost_count: u64,
    pub usd: Option<f64>,
    pub sample_start_at: Option<DateTime<Utc>>,
    pub baseline_percent: f64,
    pub sampled_percent: Option<f64>,
    pub block_count: usize,
    pub observation_count: usize,
    pub missing_token_count: u64,
    pub excluded_request_count: u64,
    pub pending_request_count: u64,
}

/// 优先预测真实的对应窗口；缺少对应周期时只给出明确标识的 7/30 天容量折算。
/// 本地日志不能证明站外消耗或完整留存，因此即使样本充足也不声称官方额度。
#[must_use]
pub fn account_quota_forecasts(
    quota: &ProviderQuota,
    account_added_at: DateTime<Utc>,
    now: DateTime<Utc>,
    samples: &[QuotaForecastSample],
) -> [AccountQuotaForecast; 2] {
    [AccountUsagePeriod::Weekly, AccountUsagePeriod::Monthly].map(|period| {
        let selected = quota
            .usage_windows()
            .find(|(_, source_period)| *source_period == period)
            .or_else(|| quota.usage_windows().min_by_key(|(_, period)| *period));
        let mut forecast = AccountQuotaForecast {
            period,
            target_seconds: match period {
                AccountUsagePeriod::Weekly => 7 * DAY_SECONDS,
                AccountUsagePeriod::Monthly => 30 * DAY_SECONDS,
            },
            extrapolated: false,
            source: None,
            unavailable_reason: Some("没有可统计的周/月额度窗口，请先刷新账号额度。"),
            low_sample: false,
            incomplete_cost: false,
            incomplete_tokens: false,
            method: QuotaForecastMethod::Cumulative,
            estimated_tokens: None,
            estimated_usd: None,
            remaining_tokens: None,
            remaining_usd: None,
        };
        if let Some((window, source_period)) = selected {
            forecast.project(
                window,
                source_period,
                quota.observed_at,
                account_added_at,
                now,
                samples.iter().find(|sample| sample.key == window.key),
            );
        }
        forecast
    })
}

impl AccountQuotaForecast {
    fn project(
        &mut self,
        window: &ProviderQuotaWindow,
        source_period: AccountUsagePeriod,
        observed_at: Option<DateTime<Utc>>,
        account_added_at: DateTime<Utc>,
        now: DateTime<Utc>,
        sample: Option<&QuotaForecastSample>,
    ) {
        let (Some(seconds), Some(reset_at)) = (window.window_seconds, window.reset_at) else {
            return;
        };
        self.extrapolated = source_period != self.period;
        if !self.extrapolated {
            self.target_seconds = seconds;
        }
        let percent = window
            .used_percent
            .filter(|p| p.is_finite() && (0.0..=100.0).contains(p));
        let usage = sample.map(|sample| &sample.usage);
        let usd = usage
            .filter(|usage| usage.known_cost_count > 0)
            .map(|usage| usage.usd)
            .filter(|value| value.is_finite() && *value >= 0.0);
        let start = i64::try_from(seconds)
            .ok()
            .and_then(Duration::try_seconds)
            .and_then(|duration| reset_at.checked_sub_signed(duration));
        self.source = Some(QuotaForecastSource {
            key: window.key.clone(),
            label: window.label.clone(),
            window_seconds: seconds,
            used_percent: percent,
            observed_at,
            start_at: start,
            reset_at,
            request_count: usage.map_or(0, |usage| usage.request_count),
            tokens: usage.map(|usage| usage.tokens),
            input_tokens: usage.map(|usage| usage.input_tokens),
            output_tokens: usage.map(|usage| usage.output_tokens),
            cached_tokens: usage.map(|usage| usage.cached_tokens),
            known_cost_count: usage.map_or(0, |usage| usage.known_cost_count),
            partial_cost_count: 0,
            unavailable_cost_count: usage.map_or(0, |usage| usage.unavailable_cost_count),
            usd,
            sample_start_at: sample.map(|sample| sample.start_at),
            baseline_percent: sample.map_or(0.0, |sample| sample.baseline_percent),
            sampled_percent: sample.map(|sample| sample.sampled_percent),
            block_count: sample.map_or(0, |sample| sample.block_count),
            observation_count: sample.map_or(0, |sample| sample.observation_count),
            missing_token_count: usage.map_or(0, |usage| usage.missing_token_count),
            excluded_request_count: usage.map_or(0, |usage| usage.excluded_request_count),
            pending_request_count: sample.map_or(0, |sample| sample.pending_request_count),
        });
        self.incomplete_cost = usage.is_none_or(|usage| {
            usage.unavailable_cost_count > 0
                || usage.known_cost_count == 0
                || usage.known_cost_count != usage.request_count
        });
        self.incomplete_tokens = usage.is_some_and(|usage| usage.missing_token_count > 0);
        self.method = sample.map_or(QuotaForecastMethod::Cumulative, |sample| sample.method);
        let Some(start) = start.filter(|start| *start <= now && now < reset_at) else {
            self.unavailable_reason = Some("额度窗口已过期或边界无效，请刷新账号额度后重试。");
            return;
        };
        if !observed_at.is_some_and(|observed| start <= observed && observed <= now) {
            self.unavailable_reason = Some("缺少本周期的额度快照，请先刷新账号额度。");
            return;
        }
        if sample.is_some_and(|sample| sample.discontinuous) {
            self.unavailable_reason =
                Some("额度观测出现回落或累计记录不连续，正在重新积累配对样本。");
            return;
        }
        if account_added_at > start && self.method == QuotaForecastMethod::Cumulative {
            self.unavailable_reason = Some(
                "本周期开始时的记录不完整，正在积累至少 5 个百分点的配对观测，无需等待下次重置。",
            );
            return;
        }
        let Some(percent) = percent else {
            self.unavailable_reason = Some("已用比例未知，请刷新额度后查看预测。");
            return;
        };
        let Some(usage) = usage.filter(|usage| usage.request_count > 0) else {
            self.unavailable_reason = Some("本周期没有网关用量记录，暂时无法预测额度。");
            return;
        };
        let Some(sample) = sample.filter(|sample| {
            sample.end_at == observed_at.unwrap_or(now)
                && start <= sample.start_at
                && sample.start_at <= sample.end_at
                && sample.sampled_percent.is_finite()
                && sample.sampled_percent >= MIN_USED_PERCENT
                && sample.sampled_percent <= percent
        }) else {
            self.unavailable_reason =
                Some("有效额度进度不足 5 个百分点或采样边界无效，请继续积累用量。");
            return;
        };
        self.low_sample = sample.sampled_percent < LOW_SAMPLE_PERCENT
            || (self.method == QuotaForecastMethod::Incremental && sample.block_count < 2);
        // 预测是近似展示值；不复用为账单金额，也不把月折算当成自然月或额外余额。
        let capacity_factor = 100.0 / sample.sampled_percent;
        let factor = capacity_factor * self.target_seconds as f64 / seconds as f64;
        let tokens = Some(usage.tokens).filter(|tokens| *tokens > 0 && !self.incomplete_tokens);
        self.estimated_tokens = tokens.and_then(|value| estimate_tokens(value, factor));
        self.estimated_usd = usd
            .filter(|_| !self.incomplete_cost)
            .and_then(|value| estimate(value, factor));
        let remaining_factor = (100.0 - percent) / sample.sampled_percent;
        self.remaining_tokens = tokens.and_then(|value| estimate_tokens(value, remaining_factor));
        self.remaining_usd = usd
            .filter(|_| !self.incomplete_cost)
            .and_then(|value| estimate(value, remaining_factor));
        self.unavailable_reason = if self.estimated_tokens.is_none() && self.estimated_usd.is_none()
        {
            Some("本周期缺少可用的 Token 或完整费用记录，暂时无法预测额度。")
        } else {
            None
        };
    }
}

fn estimate(value: f64, factor: f64) -> Option<f64> {
    let estimate = value * factor;
    (estimate.is_finite() && estimate >= 0.0).then_some(estimate)
}

fn estimate_tokens(value: u64, factor: f64) -> Option<u64> {
    estimate(value as f64, factor)
        .filter(|value| value.round() < u64::MAX as f64)
        .map(|value| value.round() as u64)
}
