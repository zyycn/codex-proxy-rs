//! 仓储组装。

use codex_proxy_adapters::{
    codex::fingerprint::FingerprintRepository,
    sqlite::{
        accounts::SqliteAccountStore, admin_sessions::SqliteAdminSessionStore,
        client_keys::SqliteClientKeyStore, cookies::SqliteCookieStore, events::SqliteEventLogStore,
        models::ModelSnapshotRepository, refresh_leases::SqliteRefreshLeaseStore,
        session_affinity::SqliteSessionAffinityStore,
    },
};
use codex_proxy_platform::{crypto::SecretBox, identity::ApiKeyHasher, storage::SqlitePool};

/// 运行时仓储集合。
#[derive(Clone)]
pub struct Repositories {
    /// 账号存储。
    pub accounts: SqliteAccountStore,
    /// 模型快照存储。
    pub model_snapshots: ModelSnapshotRepository,
    /// 客户端 API Key 存储。
    pub client_keys: SqliteClientKeyStore,
    /// 管理员会话存储。
    pub admin_sessions: SqliteAdminSessionStore,
    /// 事件日志存储。
    pub event_logs: SqliteEventLogStore,
    /// Cookie 存储。
    pub cookies: SqliteCookieStore,
    /// 指纹存储。
    pub fingerprints: FingerprintRepository,
    /// 会话亲和性存储。
    pub session_affinity: SqliteSessionAffinityStore,
    /// 账号刷新租约存储。
    pub refresh_leases: SqliteRefreshLeaseStore,
}

/// 从 SQLite 连接池构造仓储集合。
pub fn sqlite_repositories(
    pool: SqlitePool,
    secret_box: SecretBox,
    api_key_hasher: ApiKeyHasher,
) -> Repositories {
    Repositories {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box.clone()),
        model_snapshots: ModelSnapshotRepository::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), api_key_hasher),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        event_logs: SqliteEventLogStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), secret_box),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: SqliteRefreshLeaseStore::new(pool),
    }
}
