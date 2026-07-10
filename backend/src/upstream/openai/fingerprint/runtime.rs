//! OpenAI 客户端运行时指纹快照。

use std::sync::{Arc, RwLock};

use super::types::Fingerprint;

#[derive(Debug, Clone)]
pub struct RuntimeFingerprint {
    current: Arc<RwLock<Arc<Fingerprint>>>,
}

impl RuntimeFingerprint {
    pub fn new(fingerprint: Fingerprint) -> Self {
        Self {
            current: Arc::new(RwLock::new(Arc::new(fingerprint))),
        }
    }

    pub fn current(&self) -> Arc<Fingerprint> {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn snapshot(&self) -> Fingerprint {
        self.current().as_ref().clone()
    }

    pub fn replace(&self, fingerprint: Fingerprint) {
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(fingerprint);
    }
}

// ---------------------------------------------------------------------------
// 指纹历史记录与仓储
// ---------------------------------------------------------------------------
