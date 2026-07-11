//! 账号运行时反馈:错误率与 TTFT 的 EWMA 统计。
//!
//! 每个账号维护两个指数加权移动平均(EWMA):错误率与首个有效输出事件延迟(TTFT，
//! 毫秒)。请求结束后经 [`FeedbackStats::report`] 回灌，直接进 [`super::score`]
//! 打分并可驱动 sticky escape。统计以 `AtomicU64`(存 `f64` 位模式)+ CAS 无锁更新,
//! 对齐 sub2api 的设计,避免选择热路径上的锁与 DB 往返。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// EWMA 平滑系数(新样本权重)。与 sub2api 一致取 0.2。
const EWMA_ALPHA: f64 = 0.2;

/// 单账号的运行时 EWMA 统计。
///
/// `*_bits` 存 `f64` 的位模式;`NaN`(全 1 的哨兵)表示"尚无样本"。
#[derive(Debug)]
struct AccountFeedback {
    /// 错误率 EWMA，范围 `[0, 1]`。
    error_rate_bits: AtomicU64,
    /// TTFT EWMA，单位毫秒。
    ttft_ms_bits: AtomicU64,
}

/// 表示"尚无样本"的哨兵位模式。
const EMPTY_BITS: u64 = f64::NAN.to_bits();

impl Default for AccountFeedback {
    fn default() -> Self {
        Self {
            error_rate_bits: AtomicU64::new(EMPTY_BITS),
            ttft_ms_bits: AtomicU64::new(EMPTY_BITS),
        }
    }
}

impl AccountFeedback {
    /// 读取当前错误率 EWMA;无样本时返回 `None`。
    fn error_rate(&self) -> Option<f64> {
        load_sample(&self.error_rate_bits)
    }

    /// 读取当前 TTFT EWMA(毫秒);无样本时返回 `None`。
    fn ttft_ms(&self) -> Option<f64> {
        load_sample(&self.ttft_ms_bits)
    }

    /// 回灌一次请求结果。
    fn report(&self, success: bool, first_token_ms: Option<u64>) {
        let error_sample = if success { 0.0 } else { 1.0 };
        update_ewma(&self.error_rate_bits, error_sample);
        if let Some(ms) = first_token_ms {
            update_ewma(&self.ttft_ms_bits, ms as f64);
        }
    }
}

/// 读取 EWMA 样本;哨兵(NaN)返回 `None`。
fn load_sample(bits: &AtomicU64) -> Option<f64> {
    let value = f64::from_bits(bits.load(Ordering::Relaxed));
    if value.is_nan() {
        None
    } else {
        Some(value)
    }
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
    /// 错误率 EWMA，范围 `[0, 1]`;无样本时为 `None`。
    pub error_rate: Option<f64>,
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
                error_rate: feedback.error_rate(),
                ttft_ms: feedback.ttft_ms(),
            },
            None => FeedbackSample::default(),
        }
    }

    /// 回灌一次请求结果:成功/失败 + 首个有效输出事件延迟(毫秒)。
    pub fn report(&self, account_id: &str, success: bool, first_token_ms: Option<u64>) {
        // 快路径:账号已存在时只需读锁 + 原子更新。
        {
            let accounts = self
                .accounts
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(feedback) = accounts.get(account_id) {
                feedback.report(success, first_token_ms);
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
            .report(success, first_token_ms);
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
