//! 随请求存活的有界等待位置；执行容量仍由原子准入/租约端口裁决。

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::Poll;
use std::time::{Duration, Instant, SystemTime};

use futures::{FutureExt, future::poll_fn, pin_mut, select_biased, task::AtomicWaker};
use futures_timer::Delay;

const MAX_TOTAL_WAITING: usize = 1_024;
const CAPACITY_RECHECK_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConcurrencyQueuePolicy {
    pub max_waiting: u32,
    pub timeout: Duration,
}

/// 从首次入队开始计时，密钥、账号与后续重试共享同一个截止时刻。
#[derive(Debug, Clone, Default)]
pub struct ConcurrencyWaitBudget {
    deadline: Arc<OnceLock<Instant>>,
}

impl ConcurrencyWaitBudget {
    fn deadline(&self, timeout: Duration, request_deadline: SystemTime) -> Instant {
        let now = Instant::now();
        let remaining = request_deadline
            .duration_since(SystemTime::now())
            .unwrap_or_default();
        let deadline = *self.deadline.get_or_init(|| now + timeout);
        deadline.min(now + remaining)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QueueRejection {
    #[error("concurrency wait queue is full")]
    Full,
    #[error("concurrency wait deadline elapsed")]
    Timeout,
}

struct QueueState<K> {
    queues: HashMap<K, VecDeque<Arc<AtomicWaker>>>,
    total: usize,
}

/// 只保存等待者与唤醒器，既不持有请求正文，也不复制运行中并发计数。
pub struct ConcurrencyWaitQueue<K> {
    state: Arc<Mutex<QueueState<K>>>,
}

impl<K> Default for ConcurrencyWaitQueue<K> {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(QueueState {
                queues: HashMap::new(),
                total: 0,
            })),
        }
    }
}

impl<K: Clone + Eq + Hash> ConcurrencyWaitQueue<K> {
    #[must_use]
    pub fn has_waiters(&self, key: &K) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .queues
            .get(key)
            .is_some_and(|queue| !queue.is_empty())
    }

    pub fn enqueue(
        &self,
        keys: &[K],
        max_waiting: u32,
        deadline: Instant,
    ) -> Result<WaitTicket<K>, QueueRejection> {
        if Instant::now() >= deadline {
            return Err(QueueRejection::Timeout);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.total >= MAX_TOTAL_WAITING {
            return Err(QueueRejection::Full);
        }
        let key = keys
            .iter()
            .filter_map(|key| {
                let len = state.queues.get(key).map_or(0, VecDeque::len);
                (len < max_waiting as usize).then_some((key, len))
            })
            .min_by_key(|(_, len)| *len)
            .map(|(key, _)| key.clone())
            .ok_or(QueueRejection::Full)?;
        let waker = Arc::new(AtomicWaker::new());
        state
            .queues
            .entry(key.clone())
            .or_default()
            .push_back(Arc::clone(&waker));
        state.total += 1;
        Ok(WaitTicket {
            state: Arc::clone(&self.state),
            key,
            waker,
            deadline,
        })
    }
}

/// Drop 同步回收等待位置，包括尚未轮到队首的取消请求。
pub struct WaitTicket<K: Eq + Hash> {
    state: Arc<Mutex<QueueState<K>>>,
    key: K,
    waker: Arc<AtomicWaker>,
    deadline: Instant,
}

impl<K: Eq + Hash> WaitTicket<K> {
    #[must_use]
    pub const fn key(&self) -> &K {
        &self.key
    }

    pub async fn turn(&self) -> Result<(), QueueRejection> {
        let ready = poll_fn(|cx| {
            self.waker.register(cx.waker());
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state
                .queues
                .get(&self.key)
                .and_then(VecDeque::front)
                .is_some_and(|head| Arc::ptr_eq(head, &self.waker))
            {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .fuse();
        let timeout = Delay::new(self.deadline.saturating_duration_since(Instant::now())).fuse();
        pin_mut!(ready, timeout);
        select_biased! {
            _ = timeout => Err(QueueRejection::Timeout),
            () = ready => if Instant::now() < self.deadline { Ok(()) } else { Err(QueueRejection::Timeout) },
        }
    }

    pub async fn retry(&self) -> Result<(), QueueRejection> {
        // 租约释放可能经过后台队列；有界重查同时覆盖释放通知之前及 TTL 到期的空位。
        Delay::new(
            CAPACITY_RECHECK_INTERVAL.min(self.deadline.saturating_duration_since(Instant::now())),
        )
        .await;
        self.turn().await
    }
}

impl<K: Eq + Hash> Drop for WaitTicket<K> {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(queue) = state.queues.get_mut(&self.key) else {
            return;
        };
        let Some(index) = queue.iter().position(|item| Arc::ptr_eq(item, &self.waker)) else {
            return;
        };
        queue.remove(index);
        let next = queue.front().cloned();
        if queue.is_empty() {
            state.queues.remove(&self.key);
        }
        state.total -= 1;
        drop(state);
        if index == 0
            && let Some(next) = next
        {
            next.wake();
        }
    }
}

/// 单层等待位置与观测；时间预算由请求共享，重选账号不会重新开始计时。
pub struct CapacityWait<'a, K: Eq + Hash> {
    queue: &'a ConcurrencyWaitQueue<K>,
    policy: ConcurrencyQueuePolicy,
    request_deadline: SystemTime,
    budget: &'a ConcurrencyWaitBudget,
    started_at: Option<Instant>,
    ticket: Option<WaitTicket<K>>,
}

impl<'a, K: Clone + Eq + Hash> CapacityWait<'a, K> {
    #[must_use]
    pub const fn new(
        queue: &'a ConcurrencyWaitQueue<K>,
        policy: ConcurrencyQueuePolicy,
        request_deadline: SystemTime,
        budget: &'a ConcurrencyWaitBudget,
    ) -> Self {
        Self {
            queue,
            policy,
            request_deadline,
            budget,
            started_at: None,
            ticket: None,
        }
    }

    #[must_use]
    pub fn can_try(&self, key: &K) -> bool {
        self.ticket
            .as_ref()
            .is_some_and(|ticket| ticket.key() == key)
            || !self.queue.has_waiters(key)
    }

    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started_at
            .map_or(Duration::ZERO, |start| start.elapsed())
    }

    pub async fn wait(&mut self, keys: &[K]) -> Result<(), QueueRejection> {
        self.started_at.get_or_insert_with(Instant::now);
        let deadline = self
            .budget
            .deadline(self.policy.timeout, self.request_deadline);
        if self
            .ticket
            .as_ref()
            .is_some_and(|ticket| !keys.contains(ticket.key()))
        {
            self.ticket = None;
        }
        if self.ticket.is_none() {
            self.ticket = Some(
                self.queue
                    .enqueue(keys, self.policy.max_waiting, deadline)?,
            );
        }
        if let Some(ticket) = &self.ticket {
            ticket.retry().await?;
        }
        Ok(())
    }
}

impl QueueRejection {
    #[must_use]
    pub const fn provider_kind(self) -> crate::error::ProviderErrorKind {
        match self {
            Self::Full => crate::error::ProviderErrorKind::ConcurrencyQueueFull,
            Self::Timeout => crate::error::ProviderErrorKind::ConcurrencyQueueTimeout,
        }
    }
    #[must_use]
    pub fn gateway_error(self) -> crate::error::GatewayError {
        match self {
            Self::Full => crate::error::GatewayError::new(
                crate::error::GatewayErrorKind::ConcurrencyQueueFull,
                "concurrency wait queue is full",
            ),
            Self::Timeout => crate::error::GatewayError::new(
                crate::error::GatewayErrorKind::ConcurrencyQueueTimeout,
                "concurrency wait deadline elapsed",
            ),
        }
    }
}
