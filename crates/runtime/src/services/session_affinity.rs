use super::*;

/// 默认会话亲和性 TTL 秒数。
const DEFAULT_SESSION_AFFINITY_TTL_SECS: i64 = 4 * 60 * 60;

/// 运行时会话亲和性服务。
#[derive(Clone)]
pub struct RuntimeSessionAffinityService {
    store: SqliteSessionAffinityStore,
    map: Arc<tokio::sync::RwLock<SessionAffinityMap>>,
    ttl: Duration,
}

impl RuntimeSessionAffinityService {
    /// 构造运行时会话亲和性服务。
    pub fn new(store: SqliteSessionAffinityStore) -> Self {
        let ttl = Duration::seconds(DEFAULT_SESSION_AFFINITY_TTL_SECS);
        Self {
            store,
            map: Arc::new(tokio::sync::RwLock::new(SessionAffinityMap::new(ttl))),
            ttl,
        }
    }

    /// 从 SQLite 恢复未过期的会话亲和性记录。
    pub async fn restore_from_repository(
        &self,
        now: DateTime<Utc>,
    ) -> Result<usize, RuntimeSessionAffinityError> {
        let records = self.store.list_active(now).await?;
        Ok(self.map.write().await.restore(records, now))
    }

    /// 记录并持久化响应 ID 的亲和性条目。
    pub async fn record(
        &self,
        response_id: String,
        entry: SessionAffinityEntry,
    ) -> Result<(), RuntimeSessionAffinityError> {
        self.store.upsert(&response_id, &entry, self.ttl).await?;
        self.map.write().await.record(response_id, entry);
        Ok(())
    }

    /// 根据响应 ID 查找账号 ID。
    pub async fn lookup_account(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.map.read().await.lookup_account(response_id, now)
    }

    /// 根据响应 ID 查找对话 ID。
    pub async fn lookup_conversation_id(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.map
            .read()
            .await
            .lookup_conversation_id(response_id, now)
    }

    /// 根据响应 ID 查找 turn state。
    pub async fn lookup_turn_state(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.map.read().await.lookup_turn_state(response_id, now)
    }

    /// 根据响应 ID 查找指令哈希。
    pub async fn lookup_instructions_hash(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.map
            .read()
            .await
            .lookup_instructions_hash(response_id, now)
    }

    /// 根据响应 ID 查找函数调用 ID 列表。
    pub async fn lookup_function_call_ids(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Vec<String> {
        self.map
            .read()
            .await
            .lookup_function_call_ids(response_id, now)
    }

    /// 查找指定对话和变体下最新的响应 ID。
    pub async fn lookup_latest_response_by_conversation(
        &self,
        conversation_id: &str,
        max_age: Option<Duration>,
        variant_hash: Option<&str>,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.map
            .read()
            .await
            .lookup_latest_response_by_conversation(conversation_id, max_age, variant_hash, now)
    }

    /// 删除响应 ID 的内存亲和性映射。
    pub async fn forget(&self, response_id: &str) -> bool {
        self.map.write().await.forget(response_id)
    }
}

/// 运行时会话亲和性错误。
#[derive(Debug, Error)]
pub enum RuntimeSessionAffinityError {
    /// 存储访问失败。
    #[error("session affinity store error: {0}")]
    Store(#[from] SqliteSessionAffinityStoreError),
}
