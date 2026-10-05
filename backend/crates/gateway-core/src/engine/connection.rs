//! 请求内共享的发送前连接预算；不投影账号或 Provider 健康状态

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_OPENINGS: u32 = 4;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
struct State {
    started: Option<Instant>,
    openings: u32,
    completed: bool,
}

/// 换账号、换传输和内部重试共享同一预算；成功发送后的协议恢复不重置它
#[derive(Debug, Clone)]
pub struct ConnectionBudget {
    state: Arc<Mutex<State>>,
    now: fn() -> Instant,
}

impl Default for ConnectionBudget {
    fn default() -> Self {
        Self::with_clock(Instant::now)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("connection recovery budget exhausted")]
pub struct ConnectionBudgetExhausted;

impl ConnectionBudget {
    /// 绑定单调时钟；克隆预算继续共享状态和时钟，不依赖具体异步运行时
    #[must_use]
    pub fn with_clock(now: fn() -> Instant) -> Self {
        Self {
            state: Arc::default(),
            now,
        }
    }

    /// 在可能建立连接前预约一次 opening；返回本次连接允许使用的剩余时间
    /// HTTP 连接池隐藏了是否冷建连，因此一次 HTTP opening 也计入次数上限
    pub fn begin(&self) -> Result<Option<Duration>, ConnectionBudgetExhausted> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.completed {
            return Ok(None);
        }
        let started = *state.started.get_or_insert_with(self.now);
        let remaining =
            STARTUP_TIMEOUT.saturating_sub((self.now)().saturating_duration_since(started));
        if state.openings >= MAX_OPENINGS || remaining.is_zero() {
            return Err(ConnectionBudgetExhausted);
        }
        state.openings += 1;
        Ok(Some(remaining))
    }

    /// 仅连接成功不足以清零；确认业务发送后退出新增的 NotSent 恢复策略
    pub fn complete(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .completed = true;
    }

    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.completed || state.openings >= MAX_OPENINGS {
            return None;
        }
        let remaining = state.started.map_or(STARTUP_TIMEOUT, |started| {
            STARTUP_TIMEOUT.saturating_sub((self.now)().saturating_duration_since(started))
        });
        (!remaining.is_zero()).then_some(remaining)
    }

    #[must_use]
    pub fn exhausted(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !state.completed
            && (state.openings >= MAX_OPENINGS
                || state.started.is_some_and(|started| {
                    (self.now)().saturating_duration_since(started) >= STARTUP_TIMEOUT
                }))
    }

    /// 首次建连后的等待也计入恢复窗口；不缩短已发送请求的生成时间
    #[must_use]
    pub fn startup_remaining(&self) -> Option<Duration> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.completed {
            return None;
        }
        state.started.map(|started| {
            STARTUP_TIMEOUT.saturating_sub((self.now)().saturating_duration_since(started))
        })
    }

    /// 额外 attempt 最多三个；抖动由请求 ID 派生，重试不依赖运行时随机源
    #[must_use]
    pub fn retry_delay(&self, retries: u32, request_id: &str) -> Option<Duration> {
        if retries >= MAX_OPENINGS - 1 {
            return None;
        }
        let remaining = self.remaining()?;
        let hash = request_id.bytes().fold(u64::from(retries), |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(u64::from(byte))
        });
        let delay = Duration::from_millis((500_u64 << retries) * (80 + hash % 41) / 100);
        (delay < remaining).then_some(delay)
    }
}
