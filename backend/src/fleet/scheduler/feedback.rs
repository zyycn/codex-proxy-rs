//! 账号运行时 feedback：首字前失败率、首字后中断率与 TTFT 的 EWMA 统计。
//!
//! 每个账号维护三个指数加权移动平均(EWMA)。每个已启动 attempt 结束后经
//! [`FeedbackStats::report_attempt`] 回灌，直接进 [`super::score`]
//! 打分并可驱动 sticky escape。统计以 `AtomicU64`(存 `f64` 位模式)+ CAS 无锁更新,
//! 对齐 sub2api 的设计,避免选择热路径上的锁与 DB 往返。

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// EWMA 平滑系数(新样本权重)。与 sub2api 一致取 0.2。
const EWMA_ALPHA: f64 = 0.2;

/// 单个上游 attempt 对账号调度器的反馈。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptFeedback {
    Completed { first_token_ms: Option<u64> },
    Incomplete { first_token_ms: Option<u64> },
    FailedBeforeFirstToken,
    FailedAfterFirstToken { first_token_ms: u64 },
    Cancelled,
    Shutdown,
}

impl AttemptFeedback {
    fn affects_account_health(self) -> bool {
        !matches!(self, Self::Cancelled | Self::Shutdown)
    }
}

/// 单账号的运行时 EWMA 统计。
///
/// `*_bits` 存 `f64` 的位模式;`NaN`(全 1 的哨兵)表示"尚无样本"。
#[derive(Debug)]
struct AccountFeedback {
    /// 首字前失败率 EWMA，范围 `[0, 1]`。
    pre_token_failure_rate_bits: AtomicU64,
    /// 首字后中断率 EWMA，范围 `[0, 1]`。
    post_token_abort_rate_bits: AtomicU64,
    /// TTFT EWMA，单位毫秒。
    ttft_ms_bits: AtomicU64,
}

/// 表示"尚无样本"的哨兵位模式。
const EMPTY_BITS: u64 = f64::NAN.to_bits();

impl Default for AccountFeedback {
    fn default() -> Self {
        Self {
            pre_token_failure_rate_bits: AtomicU64::new(EMPTY_BITS),
            post_token_abort_rate_bits: AtomicU64::new(EMPTY_BITS),
            ttft_ms_bits: AtomicU64::new(EMPTY_BITS),
        }
    }
}

impl AccountFeedback {
    /// 读取当前首字前失败率 EWMA;无样本时返回 `None`。
    fn pre_token_failure_rate(&self) -> Option<f64> {
        load_sample(&self.pre_token_failure_rate_bits)
    }

    /// 读取当前首字后中断率 EWMA;无样本时返回 `None`。
    fn post_token_abort_rate(&self) -> Option<f64> {
        load_sample(&self.post_token_abort_rate_bits)
    }

    /// 读取当前 TTFT EWMA(毫秒);无样本时返回 `None`。
    fn ttft_ms(&self) -> Option<f64> {
        load_sample(&self.ttft_ms_bits)
    }

    /// 回灌一次已经开始的 attempt。
    fn report_attempt(&self, feedback: AttemptFeedback) {
        match feedback {
            AttemptFeedback::Completed { first_token_ms }
            | AttemptFeedback::Incomplete { first_token_ms } => {
                update_ewma(&self.pre_token_failure_rate_bits, 0.0);
                update_ewma(&self.post_token_abort_rate_bits, 0.0);
                if let Some(ms) = first_token_ms {
                    update_ewma(&self.ttft_ms_bits, ms as f64);
                }
            }
            AttemptFeedback::FailedBeforeFirstToken => {
                update_ewma(&self.pre_token_failure_rate_bits, 1.0);
            }
            AttemptFeedback::FailedAfterFirstToken { first_token_ms } => {
                update_ewma(&self.pre_token_failure_rate_bits, 0.0);
                update_ewma(&self.post_token_abort_rate_bits, 1.0);
                update_ewma(&self.ttft_ms_bits, first_token_ms as f64);
            }
            AttemptFeedback::Cancelled | AttemptFeedback::Shutdown => {}
        }
    }
}

/// 读取 EWMA 样本;哨兵(NaN)返回 `None`。
fn load_sample(bits: &AtomicU64) -> Option<f64> {
    let value = f64::from_bits(bits.load(Ordering::Relaxed));
    if value.is_nan() { None } else { Some(value) }
}

/// 无锁 CAS 更新 EWMA:首个样本直接落值,其后按 alpha 混合。
fn update_ewma(bits: &AtomicU64, sample: f64) {
    let mut current = bits.load(Ordering::Relaxed);
    loop {
        let previous = f64::from_bits(current);
        let next = if previous.is_nan() {
            sample
        } else {
            EWMA_ALPHA * sample + (1.0 - EWMA_ALPHA) * previous
        };
        match bits.compare_exchange_weak(
            current,
            next.to_bits(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

/// 账号 EWMA 反馈的读数快照,供打分使用。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FeedbackSample {
    /// 首字前失败率 EWMA，范围 `[0, 1]`;无样本时为 `None`。
    pub pre_token_failure_rate: Option<f64>,
    /// 首字后中断率 EWMA，范围 `[0, 1]`;无样本时为 `None`。
    pub post_token_abort_rate: Option<f64>,
    /// TTFT EWMA(毫秒);无样本时为 `None`。
    pub ttft_ms: Option<f64>,
}

/// 账号运行时反馈存储:按账号 ID 聚合 EWMA 统计。
///
/// 选择热路径只读(`sample`),请求结束后写(`report`)。内部按账号分片为独立的
/// 原子对象，`RwLock` 仅保护 map 结构本身(插入新账号时短暂写锁)。
#[derive(Debug, Default)]
pub struct FeedbackStats {
    accounts: RwLock<HashMap<String, AccountFeedback>>,
}

impl FeedbackStats {
    /// 创建空的反馈存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取指定账号的 EWMA 快照;账号无记录时返回全 `None`。
    pub fn sample(&self, account_id: &str) -> FeedbackSample {
        let accounts = self
            .accounts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match accounts.get(account_id) {
            Some(feedback) => FeedbackSample {
                pre_token_failure_rate: feedback.pre_token_failure_rate(),
                post_token_abort_rate: feedback.post_token_abort_rate(),
                ttft_ms: feedback.ttft_ms(),
            },
            None => FeedbackSample::default(),
        }
    }

    /// 回灌一个已启动上游 attempt 的结果。
    pub fn report_attempt(&self, account_id: &str, sample: AttemptFeedback) {
        if !sample.affects_account_health() {
            return;
        }
        // 快路径:账号已存在时只需读锁 + 原子更新。
        {
            let accounts = self
                .accounts
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(account_feedback) = accounts.get(account_id) {
                account_feedback.report_attempt(sample);
                return;
            }
        }
        // 慢路径:首次见到该账号,取写锁插入后回灌。
        let mut accounts = self
            .accounts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        accounts
            .entry(account_id.to_string())
            .or_default()
            .report_attempt(sample);
    }

    /// 移除指定账号的反馈记录(账号被删除时调用)。
    pub fn remove(&self, account_id: &str) {
        let mut accounts = self
            .accounts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        accounts.remove(account_id);
    }

    /// 清空全部反馈记录(账号池清空时调用)。
    pub fn clear(&self) {
        let mut accounts = self
            .accounts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        accounts.clear();
    }
}
