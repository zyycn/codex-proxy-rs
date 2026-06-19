//! 客户端 API Key 业务服务。

use std::sync::Arc;

use crate::admin::ports::{ClientKeyStore, ClientKeyStoreResult};

/// 客户端 API Key 服务。
#[derive(Clone)]
pub struct ClientKeyService {
    store: Arc<dyn ClientKeyStore>,
}

impl ClientKeyService {
    /// 构造服务。
    pub fn new(store: Arc<dyn ClientKeyStore>) -> Self {
        Self { store }
    }

    /// 验证客户端 API Key。
    pub async fn verify(&self, plaintext: &str) -> ClientKeyStoreResult<bool> {
        if !plaintext.starts_with("cpr_") {
            return Ok(false);
        }
        self.store.verify_and_touch(plaintext).await
    }
}
