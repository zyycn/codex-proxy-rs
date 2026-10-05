//! 随请求存活的有界等待位置；执行容量仍由原子准入/租约端口裁决

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

/// 从首次入队开始计时，密钥、账号与后续重试共享同一个截止时刻
#[derive(Debug, Clone, Default)]
pub struct ConcurrencyWaitBudget {
    deadline: Arc<OnceLock<Instant>>,
}

impl ConcurrencyWaitBudget {
    fn deadline(&self, timeout: Duration, request_deadline: Option<SystemTime>) -> Instant {
        let now = Instant::now();
        let remaining = request_deadline.map_or(timeout, |at| {
            at.duration_since(SystemTime::now()).unwrap_or_default()
        });
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

/// 同一队列内的等待类别：高优先级排在已有高优先级等待者之后、全部普通等待者之前
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaitPriority {
    #[default]
    Normal,
    High,
}

struct Waiter {
    waker: Arc<AtomicWaker>,
    priority: WaitPriority,
}

struct QueueState<K> {
    queues: HashMap<K, VecDeque<Waiter>>,
    total: usize,
}

/// 只保存等待者与唤醒器，既不持有请求正文，也不复制运行中并发计数
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

    /// 是否存在排在该类别请求之前的等待者；普通请求让位于全部等待者，高优先级只让位于同类
    fn has_waiters_ahead_of(&self, key: &K, priority: WaitPriority) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .queues
            .get(key)
            .is_some_and(|queue| {
                queue
                    .iter()
                    .any(|waiter| priority == WaitPriority::Normal || waiter.priority == priority)
            })
    }

    pub fn enqueue(
        &self,
        keys: &[K],
        max_waiting: u32,
        deadline: Instant,
    ) -> Result<WaitTicket<K>, QueueRejection> {
        self.enqueue_with_priority(keys, max_waiting, deadline, |_, waiting| -(waiting as f64))
    }

    /// 在同一锁内读取队长并入队，分数越大越优先，同分沿用候选顺序
    /// 回调只做内存评分，不得阻塞或再次访问本队列
    pub fn enqueue_with_priority(
        &self,
        keys: &[K],
        max_waiting: u32,
        deadline: Instant,
        priority: impl FnMut(&K, usize) -> f64,
    ) -> Result<WaitTicket<K>, QueueRejection> {
        self.enqueue_ranked(keys, max_waiting, deadline, WaitPriority::Normal, priority)
    }

    /// 高优先级等待者不受单队列 `max_waiting` 限制，但仍要求排队已开启并受全局容量约束
    fn enqueue_ranked(
        &self,
        keys: &[K],
        max_waiting: u32,
        deadline: Instant,
        waiter_priority: WaitPriority,
        mut priority: impl FnMut(&K, usize) -> f64,
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
                let admitted = max_waiting > 0
                    && (waiter_priority == WaitPriority::High || len < max_waiting as usize);
                admitted.then(|| (key, priority(key, len)))
            })
            .min_by(|(_, left), (_, right)| right.total_cmp(left))
            .map(|(key, _)| key.clone())
            .ok_or(QueueRejection::Full)?;
        let waker = Arc::new(AtomicWaker::new());
        let queue = state.queues.entry(key.clone()).or_default();
        let position = match waiter_priority {
            WaitPriority::Normal => queue.len(),
            WaitPriority::High => queue
                .iter()
                .position(|waiter| waiter.priority == WaitPriority::Normal)
                .unwrap_or(queue.len()),
        };
        // 插到队首时原队首失去位置，它的有界重查会自然让出；新队首随后自行轮询到位
        queue.insert(
            position,
            Waiter {
                waker: Arc::clone(&waker),
                priority: waiter_priority,
            },
        );
        state.total += 1;
        Ok(WaitTicket {
            state: Arc::clone(&self.state),
            key,
            waker,
            deadline,
        })
    }
}

/// Drop 同步回收等待位置，包括尚未轮到队首的取消请求
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

    fn is_head(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .queues
            .get(&self.key)
            .and_then(VecDeque::front)
            .is_some_and(|head| Arc::ptr_eq(&head.waker, &self.waker))
    }

    pub async fn turn(&self) -> Result<(), QueueRejection> {
        let ready = poll_fn(|cx| {
            self.waker.register(cx.waker());
            if self.is_head() {
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
        // 租约释放可能经过后台队列；有界重查同时覆盖释放通知之前及 TTL 到期的空位
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
        let Some(index) = queue
            .iter()
            .position(|item| Arc::ptr_eq(&item.waker, &self.waker))
        else {
            return;
        };
        queue.remove(index);
        let next = queue.front().map(|waiter| Arc::clone(&waiter.waker));
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

/// 单层等待位置与观测；时间预算由请求共享，重选账号不会重新开始计时
pub struct CapacityWait<'a, K: Eq + Hash> {
    queue: &'a ConcurrencyWaitQueue<K>,
    policy: ConcurrencyQueuePolicy,
    request_deadline: Option<SystemTime>,
    budget: &'a ConcurrencyWaitBudget,
    priority: WaitPriority,
    started_at: Option<Instant>,
    ticket: Option<WaitTicket<K>>,
}

impl<'a, K: Clone + Eq + Hash> CapacityWait<'a, K> {
    #[must_use]
    pub fn new(
        queue: &'a ConcurrencyWaitQueue<K>,
        policy: ConcurrencyQueuePolicy,
        request_deadline: impl Into<Option<SystemTime>>,
        budget: &'a ConcurrencyWaitBudget,
    ) -> Self {
        Self {
            queue,
            policy,
            request_deadline: request_deadline.into(),
            budget,
            priority: WaitPriority::Normal,
            started_at: None,
            ticket: None,
        }
    }

    #[must_use]
    pub const fn with_priority(mut self, priority: WaitPriority) -> Self {
        self.priority = priority;
        self
    }

    #[must_use]
    pub fn can_try(&self, key: &K) -> bool {
        if let Some(ticket) = self.ticket.as_ref().filter(|ticket| ticket.key() == key) {
            // 重读容量期间可能被高优先级请求插队，持有 ticket 不代表仍有选号资格
            ticket.is_head()
        } else {
            !self.queue.has_waiters_ahead_of(key, self.priority)
        }
    }

    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started_at
            .map_or(Duration::ZERO, |start| start.elapsed())
    }

    pub async fn wait(&mut self, keys: &[K]) -> Result<(), QueueRejection> {
        self.wait_with_priority(keys, |_, waiting| -(waiting as f64))
            .await
    }

    /// 只在首次入队或原账号不再合格时重新选队列，已取得的位置不因评分变化而移动
    pub async fn wait_with_priority(
        &mut self,
        keys: &[K],
        priority: impl FnMut(&K, usize) -> f64,
    ) -> Result<(), QueueRejection> {
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
            self.ticket = Some(self.queue.enqueue_ranked(
                keys,
                self.policy.max_waiting,
                deadline,
                self.priority,
                priority,
            )?);
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
