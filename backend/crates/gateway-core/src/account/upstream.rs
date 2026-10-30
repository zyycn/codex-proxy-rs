//! 上游互联观测与后台计划；原始协议由控制面解释，不改变账号自身额度状态

use std::{
    sync::{Arc, OnceLock, Weak},
    time::SystemTime,
};

use sha2::{Digest, Sha256};

use super::{CredentialRevision, OpaqueProviderData, ProviderAccount, ProviderAccountId};

/// 请求发出时的连接身份；出口摘要包含托管代理解析后的认证信息，但不持久化明文
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamConnection {
    pub account_id: ProviderAccountId,
    pub credential_revision: CredentialRevision,
    pub revision: String,
}

impl UpstreamConnection {
    #[must_use]
    pub fn for_account(account: &ProviderAccount) -> Self {
        Self::new(
            account.id().clone(),
            account.revision(),
            account.outbound_proxy(),
        )
    }

    #[must_use]
    pub fn new(
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
        proxy: Option<&super::OutboundProxy>,
    ) -> Self {
        let mut hash = Sha256::new();
        hash.update(credential_revision.get().to_be_bytes());
        if let Some(proxy) = proxy {
            hash.update(proxy.expose_url().as_bytes());
        }
        Self {
            account_id,
            credential_revision,
            revision: hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }
}

/// 可展示的受控同步原因，不保存上游正文、URL 或凭据
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamInfoFailure {
    Unauthorized,
    Unsupported,
    RateLimited,
    InvalidResponse,
    Unavailable,
}

impl UpstreamInfoFailure {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Unsupported => "unsupported",
            Self::RateLimited => "rate_limited",
            Self::InvalidResponse => "invalid_response",
            Self::Unavailable => "unavailable",
        }
    }
}

/// 同步元数据独立于观测正文，计划查询与领取不加载完整快照
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamInfoPlan {
    pub connection: UpstreamConnection,
    pub request_generation: u64,
    pub completed_generation: u64,
    pub next_attempt_at: SystemTime,
    pub claimed_until: Option<SystemTime>,
    pub last_attempt_at: Option<SystemTime>,
    pub last_success_at: Option<SystemTime>,
    pub last_error: Option<UpstreamInfoFailure>,
    pub consecutive_failures: u32,
}

/// 同一连接最后成功观测与独立同步计划，失败不覆盖旧观测
#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamInfoRecord {
    pub plan: UpstreamInfoPlan,
    pub snapshot: Option<OpaqueProviderData>,
}

/// 每次领取使用独立标识，计划删除重建也不能接受旧执行者的结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamInfoClaim {
    pub plan: UpstreamInfoPlan,
    pub claim_token: uuid::Uuid,
}

/// 完整观测只在连接身份、领取标识与代次仍匹配时写入
#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamInfoCompletion {
    pub connection: UpstreamConnection,
    pub request_generation: u64,
    pub claim_token: uuid::Uuid,
    pub completed_at: SystemTime,
    pub next_attempt_at: SystemTime,
    pub result: Result<OpaqueProviderData, UpstreamInfoFailure>,
}

/// 调度侧只消费已发布的中立观测，不读取协议、凭据或持久存储
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UpstreamSchedulingSignal {
    pub exhausted: bool,
    pub remaining_percent: Option<f64>,
}

pub trait UpstreamInfoSource: Send + Sync {
    fn signal(
        &self,
        account: &ProviderAccount,
        model: &str,
        now: SystemTime,
    ) -> Option<UpstreamSchedulingSignal>;
}

/// 组合根在管理服务就绪后绑定一次；弱引用避免 Provider 与管理服务互相保活
#[derive(Default)]
pub struct UpstreamInfoReader {
    source: OnceLock<Weak<dyn UpstreamInfoSource>>,
}

impl UpstreamInfoReader {
    pub fn bind(
        &self,
        source: &Arc<dyn UpstreamInfoSource>,
    ) -> Result<(), UpstreamInfoAlreadyBound> {
        self.source
            .set(Arc::downgrade(source))
            .map_err(|_| UpstreamInfoAlreadyBound)
    }
}

impl UpstreamInfoSource for UpstreamInfoReader {
    fn signal(
        &self,
        account: &ProviderAccount,
        model: &str,
        now: SystemTime,
    ) -> Option<UpstreamSchedulingSignal> {
        self.source.get()?.upgrade()?.signal(account, model, now)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("upstream information source is already bound")]
pub struct UpstreamInfoAlreadyBound;
