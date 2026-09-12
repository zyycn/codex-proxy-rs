//! 出口状态的有界生命周期；client 初始化与 JWKS 单飞共享同一淘汰边界。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::OnceCell;

pub(crate) const MAX_CACHED_EGRESS_STATES: usize = 64;

type Entries<K, V> = VecDeque<(K, Arc<OnceCell<V>>)>;

pub(crate) struct EgressCache<K, V> {
    entries: Mutex<Entries<K, V>>,
}

impl<K: PartialEq, V> EgressCache<K, V> {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
        }
    }

    pub(crate) fn with_entry(key: K, value: V) -> Self {
        Self {
            entries: Mutex::new(VecDeque::from([(
                key,
                Arc::new(OnceCell::new_with(Some(value))),
            )])),
        }
    }

    /// 初始化或 JWKS 验证期间持有 cell，避免淘汰后产生第二个单飞 owner。
    /// 推理初始化完成后仅保留 client clone；淘汰不会中断旧出口的在途请求。
    pub(crate) fn entry(&self, key: K) -> Option<Arc<OnceCell<V>>> {
        let mut entries = self.entries.lock().ok()?;
        if let Some(index) = entries.iter().position(|(candidate, _)| candidate == &key) {
            let entry = entries.remove(index)?;
            let cell = Arc::clone(&entry.1);
            entries.push_back(entry);
            return Some(cell);
        }
        if entries.len() == MAX_CACHED_EGRESS_STATES {
            // 满载且全部在用时拒绝新出口；不扩容，也不破坏既有出口的单飞。
            let idle = entries
                .iter()
                .position(|(_, value)| Arc::strong_count(value) == 1)?;
            entries.remove(idle);
        }
        let cell = Arc::new(OnceCell::new());
        entries.push_back((key, Arc::clone(&cell)));
        Some(cell)
    }
}
