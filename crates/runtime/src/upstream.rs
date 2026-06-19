//! 上游适配器组装。

use std::{sync::Arc, time::Duration};

use codex_proxy_adapters::codex::{
    client::CodexBackendClient,
    default_http_client,
    websocket::pool::{CodexWebSocketPool, CodexWebSocketPoolConfig as AdapterWebSocketPoolConfig},
};
use codex_proxy_core::gateway::{fingerprint::Fingerprint, ports::CodexModelCatalogClient};
use codex_proxy_platform::config::WebSocketPoolConfig;

/// 构造 Codex 模型目录客户端。
pub fn model_catalog_client(
    base_url: String,
    fingerprint: Fingerprint,
) -> Arc<dyn CodexModelCatalogClient> {
    Arc::new(CodexBackendClient::new(
        default_http_client(),
        base_url,
        fingerprint,
    ))
}

/// 构造 Codex 后端请求客户端。
pub fn codex_backend_client(
    base_url: String,
    fingerprint: Fingerprint,
    ws_pool: &WebSocketPoolConfig,
) -> Arc<CodexBackendClient> {
    let client = CodexBackendClient::new(default_http_client(), base_url, fingerprint);
    if ws_pool.enabled {
        Arc::new(
            client.with_websocket_pool(Arc::new(CodexWebSocketPool::with_config(
                adapter_websocket_pool_config(ws_pool),
            ))),
        )
    } else {
        Arc::new(client)
    }
}

fn adapter_websocket_pool_config(config: &WebSocketPoolConfig) -> AdapterWebSocketPoolConfig {
    AdapterWebSocketPoolConfig {
        enabled: config.enabled,
        max_age: Duration::from_millis(config.max_age_ms),
        max_per_account: config.max_per_account,
        ..AdapterWebSocketPoolConfig::default()
    }
}
