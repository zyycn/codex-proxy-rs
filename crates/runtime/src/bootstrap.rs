//! 启动编排。

use codex_proxy_adapters::codex::fingerprint::FingerprintRepository;
use codex_proxy_core::gateway::fingerprint::Fingerprint;
use codex_proxy_platform::{config::FingerprintConfig, storage::SqlitePool};

/// 从配置默认值构造指纹。
pub fn fingerprint_from_config(config: &FingerprintConfig) -> Fingerprint {
    Fingerprint {
        originator: config.originator.clone(),
        app_version: config.app_version.clone(),
        build_number: config.build_number.clone(),
        platform: config.platform.clone(),
        arch: config.arch.clone(),
        chromium_version: config.chromium_version.clone(),
        user_agent_template: config.user_agent_template.clone(),
        default_headers: config
            .default_headers
            .iter()
            .map(|header| (header.name.clone(), header.value.clone()))
            .collect(),
        header_order: config.header_order.clone(),
        updated_at: None,
    }
}

/// 从指纹仓储加载运行时当前指纹；首次启动时用配置默认值写入当前槽位。
pub async fn load_runtime_fingerprint(
    repository: &FingerprintRepository,
    default_fingerprint: &Fingerprint,
) -> Result<Fingerprint, sqlx::Error> {
    repository.ensure_current_seed(default_fingerprint).await
}

/// 从 SQLite 连接池加载运行时当前指纹。
pub async fn load_runtime_fingerprint_from_pool(
    pool: SqlitePool,
    default_fingerprint: &Fingerprint,
) -> Result<Fingerprint, sqlx::Error> {
    let repository = FingerprintRepository::new(pool);
    load_runtime_fingerprint(&repository, default_fingerprint).await
}
