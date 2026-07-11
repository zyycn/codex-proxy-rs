//! Smart 策略：加权线性打分 + EWMA 反馈。
//!
//! 参考 sub2api 的调度设计：把多个归一化因子（负载、窗口 token 用量、窗口缓存、窗口
//! 请求数、错误率、TTFT、窗口重置）按可配置权重线性求和，得分越高越优先。相较字典序
//! 元组比较，加权打分是更平滑的负载均衡，不会让单一维度无条件压制其余维度，同时通过
//! EWMA 反馈感知账号健康度与延迟。
//!
//! 各归一化因子都落在 `[0, 1]` 且方向统一（越大越好），因此权重之间可直接比较。配额
//! 已封顶（`quota_limit_reached`）的账号会被施加一个强惩罚，确保只要还有未封顶的候选就
//! 不会选中它。并列最高分的账号之间由 [`super::select_by`] 用轮转游标破并列。

use crate::fleet::account::Account;
use crate::fleet::scheduler::feedback::{FeedbackSample, FeedbackStats};
use crate::fleet::scheduler::{account_window_token_count, SelectionInput};

/// 配额封顶账号的惩罚分。远大于各因子加权和的理论上限（各因子 ∈ [0,1]，权重和通常为
/// 个位数），保证未封顶账号总是优先于封顶账号。
const QUOTA_LIMITED_PENALTY: f64 = 1_000.0;

/// 打分权重。每个字段对应一个归一化因子的权重，权重为 0 即关闭该维度。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreWeights {
    /// 在途槽位压力（越空闲越优先）。
    pub load: f64,
    /// 当前窗口实际 token 用量（用得越少越优先）。
    pub window: f64,
    /// 当前窗口缓存 token 用量（缓存越少越优先，权重小，仅在实际用量相当时区分）。
    pub cached: f64,
    /// 当前窗口请求数（请求越少越优先）。
    pub window_requests: f64,
    /// 错误率 EWMA（越健康越优先）。
    pub error_rate: f64,
    /// 首个有效输出事件延迟 EWMA（越快越优先）。
    pub ttft: f64,
    /// 窗口重置临近度（越快重置越优先，use-it-or-lose-it，默认关闭）。
    pub reset: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        // 默认权重对齐 sub2api 的取向：负载与健康度为主，窗口用量次之，TTFT 再次，
        // 窗口重置默认关闭（避免过早榨干即将重置的账号）。cached 权重远小于 window，
        // 确保实际 token 用量主导、缓存仅作细粒度区分。
        Self {
            load: 1.0,
            window: 0.8,
            cached: 0.05,
            window_requests: 0.6,
            error_rate: 1.0,
            ttft: 0.5,
            reset: 0.0,
        }
    }
}

/// 单个候选账号的打分明细，供日志与可观测使用。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreBreakdown {
    /// 各因子加权后的总分（已含配额封顶惩罚）。
    pub total: f64,
    /// 负载因子（归一化后，未乘权重）。
    pub load: f64,
    /// 窗口实际 token 用量因子。
    pub window: f64,
    /// 窗口缓存 token 用量因子。
    pub cached: f64,
    /// 窗口请求数因子。
    pub window_requests: f64,
    /// 错误率因子。
    pub error_rate: f64,
    /// TTFT 因子。
    pub ttft: f64,
    /// 窗口重置因子。
    pub reset: f64,
    /// 是否被施加配额封顶惩罚。
    pub quota_limited: bool,
}

/// Smart 策略选择：按加权得分降序排序，在并列最高分之间用轮转游标破并列。
pub fn select(input: &SelectionInput<'_>, cursor: &mut usize) -> Option<Account> {
    let ranked = rank_candidate_refs(
        input.candidates,
        input.weights,
        input.now.timestamp(),
        input.slot_count,
        input.feedback,
    );
    let best_total = ranked.first()?.1.total;
    // 并列最高分的账号数量（浮点相等用位相等判定，得分由相同输入算出，稳定）。
    let tied_count = ranked
        .iter()
        .take_while(|(_, breakdown)| breakdown.total.to_bits() == best_total.to_bits())
        .count();
    let index = *cursor % tied_count;
    *cursor = cursor.wrapping_add(1);
    let account_index = ranked[index].0;
    Some((*input.candidates[account_index]).clone())
}

/// 按 Smart 得分为完整候选集排序；同分账号沿用调度器轮转游标破并列。
pub fn order(input: &SelectionInput<'_>, cursor: &mut usize) -> Vec<Account> {
    let mut ranked = rank_candidate_refs(
        input.candidates,
        input.weights,
        input.now.timestamp(),
        input.slot_count,
        input.feedback,
    );
    let rotation = *cursor;
    *cursor = cursor.wrapping_add(1);

    let mut start = 0;
    while start < ranked.len() {
        let score = ranked[start].1.total.to_bits();
        let mut end = start + 1;
        while end < ranked.len() && ranked[end].1.total.to_bits() == score {
            end += 1;
        }
        ranked[start..end].rotate_left(rotation % (end - start));
        start = end;
    }

    ranked
        .into_iter()
        .map(|(index, _)| (*input.candidates[index]).clone())
        .collect()
}

/// 对候选集合按加权得分排序（降序），返回 `(账号索引, 打分明细)` 列表。
///
/// 只做打分与排序，最终的并列破并列交给 [`select`]。公开供测试直接断言排序结果。
pub fn rank_candidates(
    candidates: &[Account],
    weights: &ScoreWeights,
    now_secs: i64,
    slot_count: &dyn Fn(&str) -> usize,
    feedback: &FeedbackStats,
) -> Vec<(usize, ScoreBreakdown)> {
    let candidates = candidates.iter().collect::<Vec<_>>();
    rank_candidate_refs(&candidates, weights, now_secs, slot_count, feedback)
}

fn rank_candidate_refs(
    candidates: &[&Account],
    weights: &ScoreWeights,
    now_secs: i64,
    slot_count: &dyn Fn(&str) -> usize,
    feedback: &FeedbackStats,
) -> Vec<(usize, ScoreBreakdown)> {
    let samples = candidates
        .iter()
        .map(|account| feedback.sample(&account.id))
        .collect::<Vec<_>>();
    let ctx = NormalizationContext::build(candidates, &samples, now_secs, slot_count);
    let mut ranked = candidates
        .iter()
        .zip(&samples)
        .enumerate()
        .map(|(index, (account, sample))| {
            (
                index,
                score_account(account, sample, weights, &ctx, now_secs, slot_count),
            )
        })
        .collect::<Vec<_>>();
    // 降序：得分高在前。NaN 不应出现（因子均有界），保守用 total_cmp。
    ranked.sort_by(|a, b| b.1.total.total_cmp(&a.1.total));
    ranked
}

/// 候选集合的归一化上下文：预先算好各维度的 min/max，供逐账号归一化。
struct NormalizationContext {
    max_slot_count: f64,
    max_window_tokens: f64,
    max_window_cached: f64,
    max_window_requests: f64,
    min_ttft_ms: f64,
    max_ttft_ms: f64,
    min_reset_secs: f64,
    max_reset_secs: f64,
}

impl NormalizationContext {
    fn build(
        candidates: &[&Account],
        samples: &[FeedbackSample],
        now_secs: i64,
        slot_count: &dyn Fn(&str) -> usize,
    ) -> Self {
        let mut max_slot_count = 0.0_f64;
        let mut max_window_tokens = 0.0_f64;
        let mut max_window_cached = 0.0_f64;
        let mut max_window_requests = 0.0_f64;
        let mut min_ttft_ms = f64::INFINITY;
        let mut max_ttft_ms = 0.0_f64;
        let mut min_reset_secs = f64::INFINITY;
        let mut max_reset_secs = 0.0_f64;

        for (account, sample) in candidates.iter().zip(samples) {
            max_slot_count = max_slot_count.max(slot_count(&account.id) as f64);
            max_window_tokens = max_window_tokens.max(account_window_token_count(account) as f64);
            max_window_cached = max_window_cached.max(account.window_cached_tokens as f64);
            max_window_requests = max_window_requests.max(account.window_request_count as f64);

            if let Some(ttft) = sample.ttft_ms {
                if ttft > 0.0 {
                    min_ttft_ms = min_ttft_ms.min(ttft);
                    max_ttft_ms = max_ttft_ms.max(ttft);
                }
            }

            if let Some(reset_at) = account.window_reset_at {
                let secs = (reset_at.timestamp() - now_secs).max(0) as f64;
                min_reset_secs = min_reset_secs.min(secs);
                max_reset_secs = max_reset_secs.max(secs);
            }
        }

        Self {
            max_slot_count,
            max_window_tokens,
            max_window_cached,
            max_window_requests,
            min_ttft_ms: if min_ttft_ms.is_finite() {
                min_ttft_ms
            } else {
                0.0
            },
            max_ttft_ms,
            min_reset_secs: if min_reset_secs.is_finite() {
                min_reset_secs
            } else {
                0.0
            },
            max_reset_secs,
        }
    }
}

/// 把 `value` 从 `[min, max]` 线性映射到 `[0, 1]`；区间退化时返回中性值 `1.0`
/// （表示该维度对所有候选无区分度，不应影响排序）。
fn normalize_ascending_good(value: f64, min: f64, max: f64) -> f64 {
    if max <= min {
        return 1.0;
    }
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

/// 计算指定账号在候选集合中的加权得分明细。
fn score_account(
    account: &Account,
    sample: &FeedbackSample,
    weights: &ScoreWeights,
    ctx: &NormalizationContext,
    now_secs: i64,
    slot_count: &dyn Fn(&str) -> usize,
) -> ScoreBreakdown {
    // 负载：槽位越少越好。归一化到 [0,1] 后取反（1 - x），使“越空闲得分越高”。
    let load =
        1.0 - normalize_ascending_good(slot_count(&account.id) as f64, 0.0, ctx.max_slot_count);

    // 窗口实际 token 用量：token 越少越好。
    let window = 1.0
        - normalize_ascending_good(
            account_window_token_count(account) as f64,
            0.0,
            ctx.max_window_tokens,
        );

    // 窗口缓存 token 用量：缓存越少越好（权重小，仅在实际用量相当时细分）。
    let cached = 1.0
        - normalize_ascending_good(
            account.window_cached_tokens as f64,
            0.0,
            ctx.max_window_cached,
        );

    // 窗口请求数：请求越少越好。
    let window_requests = 1.0
        - normalize_ascending_good(
            account.window_request_count as f64,
            0.0,
            ctx.max_window_requests,
        );

    // 错误率 EWMA：本身即 [0,1]，越低越健康。无样本时视为 0（满分健康）。
    let error_rate = 1.0 - sample.error_rate.unwrap_or(0.0).clamp(0.0, 1.0);

    // TTFT：延迟越低越好。无样本的账号视为中性满分，避免冷启动被压。
    let ttft = match sample.ttft_ms {
        Some(ms) if ms > 0.0 => {
            1.0 - normalize_ascending_good(ms, ctx.min_ttft_ms, ctx.max_ttft_ms)
        }
        _ => 1.0,
    };

    // 窗口重置：越快重置越优先（use-it-or-lose-it）。无 reset 时间视为中性满分。
    let reset = match account.window_reset_at {
        Some(reset_at) => {
            let secs = (reset_at.timestamp() - now_secs).max(0) as f64;
            1.0 - normalize_ascending_good(secs, ctx.min_reset_secs, ctx.max_reset_secs)
        }
        None => 1.0,
    };

    let mut total = weights.load * load
        + weights.window * window
        + weights.cached * cached
        + weights.window_requests * window_requests
        + weights.error_rate * error_rate
        + weights.ttft * ttft
        + weights.reset * reset;

    // 配额封顶硬惩罚：确保只要还有未封顶候选就不会选中封顶账号。
    if account.quota_limit_reached {
        total -= QUOTA_LIMITED_PENALTY;
    }

    ScoreBreakdown {
        total,
        load,
        window,
        cached,
        window_requests,
        error_rate,
        ttft,
        reset,
        quota_limited: account.quota_limit_reached,
    }
}
