use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::interval;

/// 会话亲和性条目 - 记录响应ID到账户的映射
#[derive(Debug, Clone)]
pub struct AffinityEntry {
    /// 账户ID
    pub account_id: String,
    /// 对话ID
    pub conversation_id: String,
    /// 上游 turn state token
    pub turn_state: Option<String>,
    /// 指令哈希（SHA-256）- 用于缓存命中检测
    pub instructions_hash: Option<String>,
    /// 输入token数
    pub input_tokens: Option<u64>,
    /// 函数调用ID列表
    pub function_call_ids: Vec<String>,
    /// 变体哈希 - 用于区分并发对话分支
    pub variant_hash: Option<String>,
    /// 创建时间
    pub created_at: Instant,
}

/// 会话亲和性映射 - 将 previous_response_id 映射到创建它的账户
///
/// 功能：
/// - 支持服务器端对话历史重用
/// - 提示缓存命中（缓存按账户存储在后端）
/// - 4小时TTL
/// - 每10分钟自动清理过期条目
pub struct SessionAffinityMap {
    map: Arc<RwLock<HashMap<String, AffinityEntry>>>,
    ttl: Duration,
    cleanup_handle: Option<tokio::task::JoinHandle<()>>,
}

const DEFAULT_TTL: Duration = Duration::from_secs(4 * 60 * 60); // 4小时
const CLEANUP_INTERVAL: Duration = Duration::from_secs(10 * 60); // 10分钟

impl SessionAffinityMap {
    pub fn new(ttl: Duration) -> Self {
        Self {
            map: Arc::new(RwLock::new(HashMap::new())),
            ttl,
            cleanup_handle: None,
        }
    }

    pub fn with_default_ttl() -> Self {
        Self::new(DEFAULT_TTL)
    }

    /// 启动自动清理任务
    pub fn start_cleanup(&mut self) {
        if self.cleanup_handle.is_some() {
            return; // 已经启动
        }

        let map = self.map.clone();
        let ttl = self.ttl;

        let handle = tokio::spawn(async move {
            let mut ticker = interval(CLEANUP_INTERVAL);
            loop {
                ticker.tick().await;
                let now = Instant::now();
                let mut map_lock = map.write().await;
                map_lock.retain(|_, entry| now.duration_since(entry.created_at) < ttl);
            }
        });

        self.cleanup_handle = Some(handle);
    }

    /// 停止自动清理任务
    pub fn stop_cleanup(&mut self) {
        if let Some(handle) = self.cleanup_handle.take() {
            handle.abort();
        }
    }

    /// 记录响应ID到账户的映射
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        response_id: String,
        account_id: String,
        conversation_id: String,
        turn_state: Option<String>,
        instructions: Option<&str>,
        input_tokens: Option<u64>,
        function_call_ids: Option<Vec<String>>,
        variant_hash: Option<String>,
    ) {
        let instructions_hash = instructions.map(|s| {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(s.as_bytes());
            let result = hasher.finalize();
            hex::encode(result)
        });

        let entry = AffinityEntry {
            account_id,
            conversation_id,
            turn_state,
            instructions_hash,
            input_tokens,
            function_call_ids: function_call_ids.unwrap_or_default(),
            variant_hash,
            created_at: Instant::now(),
        };

        self.map.write().await.insert(response_id, entry);
    }

    /// 查找响应ID对应的账户ID
    pub async fn lookup_account(&self, response_id: &str) -> Option<String> {
        self.get_entry(response_id).await.map(|e| e.account_id)
    }

    /// 查找响应ID对应的对话ID
    pub async fn lookup_conversation_id(&self, response_id: &str) -> Option<String> {
        self.get_entry(response_id).await.map(|e| e.conversation_id)
    }

    /// 查找对话ID的最新响应ID
    ///
    /// 参数：
    /// - conversation_id: 对话ID
    /// - max_age: 最大年龄（用于避免使用已过期的提示缓存）
    /// - variant_hash: 变体哈希（用于区分并发分支）
    pub async fn lookup_latest_response_by_conversation(
        &self,
        conversation_id: &str,
        max_age: Option<Duration>,
        variant_hash: Option<&str>,
    ) -> Option<String> {
        let map = self.map.read().await;
        let now = Instant::now();

        let mut latest_response_id: Option<String> = None;
        let mut latest_created_at = Instant::now() - Duration::from_secs(u64::MAX / 2);

        for (response_id, entry) in map.iter() {
            // 过滤对话ID
            if entry.conversation_id != conversation_id {
                continue;
            }

            // 过滤变体哈希
            if let Some(vh) = variant_hash {
                if entry.variant_hash.as_deref() != Some(vh) {
                    continue;
                }
            }

            // 检查TTL
            if now.duration_since(entry.created_at) >= self.ttl {
                continue;
            }

            // 检查最大年龄
            if let Some(max) = max_age {
                if now.duration_since(entry.created_at) > max {
                    continue;
                }
            }

            // 保留最新的
            if entry.created_at > latest_created_at {
                latest_created_at = entry.created_at;
                latest_response_id = Some(response_id.clone());
            }
        }

        latest_response_id
    }

    /// 查找响应ID对应的 turn state
    pub async fn lookup_turn_state(&self, response_id: &str) -> Option<String> {
        self.get_entry(response_id).await.and_then(|e| e.turn_state)
    }

    /// 查找响应ID对应的指令哈希
    pub async fn lookup_instructions_hash(&self, response_id: &str) -> Option<String> {
        self.get_entry(response_id)
            .await
            .and_then(|e| e.instructions_hash)
    }

    /// 查找响应ID对应的输入token数
    pub async fn lookup_input_tokens(&self, response_id: &str) -> Option<u64> {
        self.get_entry(response_id)
            .await
            .and_then(|e| e.input_tokens)
    }

    /// 查找响应ID对应的函数调用ID列表
    pub async fn lookup_function_call_ids(&self, response_id: &str) -> Vec<String> {
        self.get_entry(response_id)
            .await
            .map(|e| e.function_call_ids)
            .unwrap_or_default()
    }

    /// 删除响应ID（上游返回 not-found 时调用）
    pub async fn forget(&self, response_id: &str) {
        self.map.write().await.remove(response_id);
    }

    /// 获取映射大小
    pub async fn size(&self) -> usize {
        self.map.read().await.len()
    }

    /// 清空所有映射
    pub async fn clear(&self) {
        self.map.write().await.clear();
    }

    /// 获取条目（检查TTL）
    async fn get_entry(&self, response_id: &str) -> Option<AffinityEntry> {
        let map = self.map.read().await;
        let entry = map.get(response_id)?;

        let now = Instant::now();
        if now.duration_since(entry.created_at) >= self.ttl {
            return None;
        }

        Some(entry.clone())
    }
}

impl Drop for SessionAffinityMap {
    fn drop(&mut self) {
        self.stop_cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_affinity_records_and_retrieves_account() {
        let map = SessionAffinityMap::with_default_ttl();

        map.record(
            "resp_123".to_string(),
            "acc_456".to_string(),
            "conv_789".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        let account = map.lookup_account("resp_123").await;
        assert_eq!(account, Some("acc_456".to_string()));
    }

    #[tokio::test]
    async fn session_affinity_returns_none_for_unknown_response() {
        let map = SessionAffinityMap::with_default_ttl();
        let account = map.lookup_account("unknown").await;
        assert_eq!(account, None);
    }

    #[tokio::test]
    async fn session_affinity_expires_old_entries() {
        let map = SessionAffinityMap::new(Duration::from_millis(100));

        map.record(
            "resp_123".to_string(),
            "acc_456".to_string(),
            "conv_789".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        // 立即查找应该成功
        assert!(map.lookup_account("resp_123").await.is_some());

        // 等待过期
        tokio::time::sleep(Duration::from_millis(150)).await;

        // 过期后应该返回 None
        assert!(map.lookup_account("resp_123").await.is_none());
    }

    #[tokio::test]
    async fn session_affinity_finds_latest_response_by_conversation() {
        let map = SessionAffinityMap::with_default_ttl();

        map.record(
            "resp_1".to_string(),
            "acc_1".to_string(),
            "conv_789".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        tokio::time::sleep(Duration::from_millis(10)).await;

        map.record(
            "resp_2".to_string(),
            "acc_1".to_string(),
            "conv_789".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        let latest = map
            .lookup_latest_response_by_conversation("conv_789", None, None)
            .await;
        assert_eq!(latest, Some("resp_2".to_string()));
    }

    #[tokio::test]
    async fn session_affinity_forget_removes_entry() {
        let map = SessionAffinityMap::with_default_ttl();

        map.record(
            "resp_123".to_string(),
            "acc_456".to_string(),
            "conv_789".to_string(),
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        assert!(map.lookup_account("resp_123").await.is_some());

        map.forget("resp_123").await;

        assert!(map.lookup_account("resp_123").await.is_none());
    }
}
