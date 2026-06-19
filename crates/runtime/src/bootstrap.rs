//! 启动编排。

use codex_proxy_adapters::codex::fingerprint::FingerprintRepository;
use codex_proxy_core::gateway::fingerprint::Fingerprint;

/// 从指纹仓储加载运行时指纹，必要时回退到默认值。
pub async fn load_runtime_fingerprint(repository: &FingerprintRepository) -> Fingerprint {
    match repository.load_latest_auto_updated().await {
        Ok(Some(fingerprint)) => fingerprint,
        _ => Fingerprint::default_codex_desktop(),
    }
}
