//! Client Key 对互联节点公开本地观测的逐项授权

use serde::{Deserialize, Serialize};

/// 公开权限默认关闭，不因协议识别或预算查询自动开启
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientPeerSharing {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub share_pool: bool,
    #[serde(default)]
    pub share_quota: bool,
    #[serde(default)]
    pub share_plans: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientPeerSharingError {
    MissingGroupScope,
    PlansWithoutPool,
}

impl ClientPeerSharing {
    pub const fn validate(self, has_bound_groups: bool) -> Result<(), ClientPeerSharingError> {
        if self.enabled && !has_bound_groups {
            return Err(ClientPeerSharingError::MissingGroupScope);
        }
        if self.share_plans && !self.share_pool {
            return Err(ClientPeerSharingError::PlansWithoutPool);
        }
        Ok(())
    }
}
