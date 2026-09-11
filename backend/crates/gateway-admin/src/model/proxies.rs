//! Outbound proxy pool commands and projections.
//!
//! 完整代理 URL 只经显式 reveal 返回一次；常规列表投影仅暴露脱敏 endpoint。

use chrono::{DateTime, Utc};
use gateway_core::account::OutboundProxy;

use super::{PageSize, Revision};

/// Validated `pxy_<uuid>` outbound proxy identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutboundProxyId(String);

/// 出站代理 ID 不满足 `pxy_<32 hex>` 形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid outbound proxy ID")]
pub struct InvalidOutboundProxyId;

impl OutboundProxyId {
    /// 校验 `pxy_<32 lowercase hex>` 形态的代理 ID。
    ///
    /// # Errors
    ///
    /// 形态不符时返回 [`InvalidOutboundProxyId`]。
    pub fn new(value: String) -> Result<Self, InvalidOutboundProxyId> {
        let hex = value.strip_prefix("pxy_").ok_or(InvalidOutboundProxyId)?;
        if hex.len() == 32
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value))
        } else {
            Err(InvalidOutboundProxyId)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Complete outbound proxy summary; the URL stays inside [`OutboundProxy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundProxyRecord {
    pub id: OutboundProxyId,
    pub name: String,
    pub proxy: OutboundProxy,
    /// 当前把该代理 URL 设为出站隧道的账号数量。
    pub account_count: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Outbound proxy list query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundProxyListQuery {
    pub page: u32,
    pub page_size: PageSize,
    pub search: Option<String>,
}

/// Paginated outbound proxies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundProxyPage {
    pub config_revision: Revision,
    pub items: Vec<OutboundProxyRecord>,
    pub total: u64,
    pub page: u32,
    pub page_size: u16,
}

/// Create an outbound proxy pool entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOutboundProxy {
    pub name: String,
    pub proxy: OutboundProxy,
}

/// Store-ready create command with a generated stable ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutboundProxy {
    pub id: OutboundProxyId,
    pub name: String,
    pub proxy: OutboundProxy,
}

/// Update an outbound proxy; `proxy` 为 `None` 时保留当前 URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutboundProxy {
    pub id: OutboundProxyId,
    pub name: String,
    pub proxy: Option<OutboundProxy>,
}

/// Delete an outbound proxy pool entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteOutboundProxy {
    pub id: OutboundProxyId,
}

/// Result of any outbound proxy mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundProxyMutation {
    pub config_revision: Revision,
    pub id: OutboundProxyId,
    pub record: Option<OutboundProxyRecord>,
}

/// 仅由显式 reveal 返回一次的完整代理 URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevealedOutboundProxy {
    pub id: OutboundProxyId,
    pub proxy: OutboundProxy,
}

/// 连通性测试对象：池内代理或未保存的临时代理 URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundProxyTestSubject {
    Saved(OutboundProxyId),
    Inline(OutboundProxy),
}

/// 连通性测试命令；`target_url` 为空时使用服务端默认目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOutboundProxy {
    pub subject: OutboundProxyTestSubject,
    pub target_url: Option<String>,
}

/// 已脱敏的连通性测试结果；错误消息不回显代理 URL 或凭据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundProxyTestReport {
    pub success: bool,
    pub latency_ms: u64,
    pub status_code: Option<u16>,
    pub target_url: String,
    pub error: Option<String>,
}
