//! 账号连接身份与上游调度只读合同，原始协议由控制面解释

use std::{
    sync::{Arc, OnceLock, Weak},
    time::SystemTime,
};

use sha2::{Digest, Sha256};

use super::{CredentialRevision, ProviderAccount, ProviderAccountId};

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
    SharingDisabled,
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
            Self::SharingDisabled => "sharing_disabled",
            Self::Unsupported => "unsupported",
            Self::RateLimited => "rate_limited",
            Self::InvalidResponse => "invalid_response",
            Self::Unavailable => "unavailable",
        }
    }
}

/// 调度侧只消费已发布的中立观测，不读取协议、凭据或持久存储
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UpstreamSchedulingSignal {
    pub concurrency_limit: Option<super::AccountConcurrencyLimit>,
    pub exhausted: bool,
    pub remaining_percent: Option<f64>,
}

pub trait UpstreamInfoSource: Send + Sync {
    /// 当前连接是否已识别为互联节点，不以额度快照的新鲜度代替身份
    fn is_peer(&self, account: &ProviderAccount) -> bool;

    fn signal(
        &self,
        account: &ProviderAccount,
        model: Option<&str>,
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
    fn is_peer(&self, account: &ProviderAccount) -> bool {
        self.source
            .get()
            .and_then(Weak::upgrade)
            .is_some_and(|source| source.is_peer(account))
    }

    fn signal(
        &self,
        account: &ProviderAccount,
        model: Option<&str>,
        now: SystemTime,
    ) -> Option<UpstreamSchedulingSignal> {
        self.source.get()?.upgrade()?.signal(account, model, now)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("upstream information source is already bound")]
pub struct UpstreamInfoAlreadyBound;
