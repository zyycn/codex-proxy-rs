//! Codex quota 门面与唯一服务状态；实现按读取、排程、预热和重置消费组织
//!
//! - [`document`]：上游 `/usage` 输入到多桶 map 的单向规范化
//! - [`snapshot`]：快照/窗口解析、聚合、O(1) 滚动与调度信号
//! - [`evidence`]：额度接口错误到凭据/额度事实的分类
//! - [`recovery`]：各额度窗口独立的恢复基准与访问结论

mod document;
pub(crate) mod evidence;
mod passive;
mod recovery;
mod reset_credits;
mod scheduling;
mod service;
pub(crate) mod snapshot;
mod warmup;

pub use reset_credits::CodexResetCreditsError;
use reset_credits::ResetCreditLocks;
use scheduling::CodexQuotaSchedulingProjection;

pub use snapshot::{
    CodexAccountQuotaSnapshot, CodexQuotaFact, CodexQuotaWindow, CodexQuotaWindowKind,
    CodexQuotaWindowRole, parse_codex_quota_usage,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use futures::StreamExt as _;
use gateway_core::account::{
    AccountErrorReason, AccountQuotaSignals, CredentialRevision, CredentialState,
    OpaqueProviderData, ProviderAccount, ProviderAccountId, ProviderAccountStore,
    QuotaAccessChange, QuotaAccessState, QuotaEvidence, QuotaObservation, QuotaObservationTouch,
    QuotaState, QuotaWriteOutcome,
};
use gateway_core::provider_ports::{
    ProviderCooldown, ProviderCooldownPort, ProviderRuntimePolicyPort,
};
use gateway_protocol::openai::events::{
    ParsedRateLimits, RateLimitDetails, RateLimitWindow, parse_rate_limit_headers,
};
use gateway_protocol::openai::sse::{SseEvent, SseEventDecoder};
use reqwest::Client;
use secrecy::ExposeSecret;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::transport::profile::CodexWireProfileState;
use crate::transport::protocol::responses::CodexResponsesRequest;
use crate::transport::{
    CodexBackendClient, CodexBackendStreamingResponse, CodexClientError,
    CodexRateLimitResetCredits, CodexRateLimitResetCreditsConsumeResult, CodexRequestContext,
};

use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::security::CodexRuntimeCredential;
use document::{
    DEFAULT_CODEX_LIMIT_ID, RateLimitSnapshotsByLimitId, normalize_quota_window_placeholders,
    observed_account_plan,
};
use evidence::{QuotaEndpointFailure, classify_quota_endpoint_failure};
use recovery::{RECOVERY_FIELD, reconcile_refresh};
use snapshot::{
    parse_account_quota_snapshot, quota_projection_ttl, quota_snapshot_from_observation,
    scheduling_signals_from_snapshot,
};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
pub(crate) const QUOTA_SCHEDULING_TTL: Duration = Duration::from_secs(10 * 60);
const QUOTA_HYDRATION_FAILURE_TTL: Duration = Duration::from_secs(5);
const PERIODIC_QUOTA_REFRESH_RETRY_INTERVAL: Duration = Duration::from_secs(30 * 60);
const QUOTA_RESET_FAST_REFRESH_WINDOW: Duration = Duration::from_secs(5 * 60);
const QUOTA_RESET_FAST_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const QUOTA_RESET_REFRESH_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
const EXHAUSTED_QUOTA_RESET_GRACE: Duration = Duration::from_secs(10);
const QUOTA_RESET_GRACE: Duration = Duration::from_secs(2 * 60);
/// 首次 OAuth 异步观察失败时，由既有 quota worker 兜底重试的单轮上限
/// 随机启动延迟已把候选摊到多轮，该上限仅作单轮安全边界
const INITIAL_QUOTA_SYNC_BATCH: usize = 100;
/// 新账号首查（含失败重排）随机启动延迟的上限；摊开导入批量的 /usage 突发
/// 导入即时可见额度由导入流程的每账号后台读取负责，worker 只兜底失败者
const INITIAL_QUOTA_SYNC_MAX_START_DELAY: Duration = Duration::from_secs(10 * 60);
// 5xx 上游拒绝的短退避重试预算；指数退避 1s/2s，吞掉瞬时抖动
const QUOTA_FETCH_5XX_MAX_RETRIES: u32 = 2;
const QUOTA_FETCH_5XX_BASE_DELAY: Duration = Duration::from_secs(1);
const WARMUP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const WARMUP_STREAM_MAX_BYTES: usize = 256 * 1024;

/// OpenAI Provider 主动额度刷新的调度策略
///
/// 正常账号依赖请求响应的被动额度同步；周期 worker 仅复核已耗尽账号
/// 该策略保留模型目录的周期刷新频率；额度到期检查使用独立的短周期，
/// 避免到达 reset 后还要等待完整的目录刷新周期
#[derive(Debug, Clone, Copy)]
pub struct CodexQuotaRefreshPolicy {
    interval: Duration,
}

impl CodexQuotaRefreshPolicy {
    #[must_use]
    pub const fn new(interval: Duration) -> Self {
        Self { interval }
    }

    #[must_use]
    pub const fn interval(self) -> Duration {
        self.interval
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexQuotaSyncSummary {
    pub updated: u64,
    pub exhausted: u64,
    pub banned: u64,
    pub transient: u64,
    pub stale: u64,
}

impl CodexQuotaSyncSummary {
    #[must_use]
    pub const fn has_operational_failures(self) -> bool {
        self.transient > 0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexWarmupSummary {
    pub warmed_up: u64,
    pub skipped_active: u64,
    pub skipped_exhausted: u64,
    pub failed: u64,
}

#[derive(Debug, Error)]
pub enum CodexCredentialQuotaError {
    #[error("Codex quota response is invalid")]
    InvalidCredentialData,
    #[error("Codex OAuth access token must be refreshed before querying quota")]
    CredentialRefreshRequired,
    #[error(transparent)]
    Repository(#[from] CredentialRepositoryError),
    #[error("provider account store is unavailable: {detail}")]
    Store { detail: String },
    #[error("Codex quota account was not found")]
    NotFound,
    #[error("Codex quota credential revision is stale")]
    RevisionConflict,
    #[error("Codex quota upstream query failed: {detail}")]
    Upstream {
        detail: String,
        /// 上游 HTTP 状态码；传输失败等无响应场景为 `None`
        status: Option<u16>,
        /// 有界错误码，供内部诊断和已知码映射使用，不直接拼入公开提示
        code: Option<String>,
    },
}

impl From<gateway_core::error::StoreError> for CodexCredentialQuotaError {
    fn from(error: gateway_core::error::StoreError) -> Self {
        Self::Store {
            detail: error.to_string(),
        }
    }
}

pub struct CodexCredentialQuotaService {
    repository: CodexCredentialRepository,
    store: Arc<dyn ProviderAccountStore>,
    profile: CodexWireProfileState,
    http: Client,
    base_url: String,
    cooldowns: Arc<dyn ProviderCooldownPort>,
    runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
    scheduling: CodexQuotaSchedulingProjection,
    reset_consume_locks: ResetCreditLocks,
    /// 首查随机延迟采样器；测试注入固定值获得确定性节奏。
    initial_sync_delays: CodexInitialSyncDelays,
}

/// 首查随机延迟采样器：每次调用为一个账号返回独立随机延迟
#[doc(hidden)]
pub type CodexInitialSyncDelays = Arc<dyn Fn() -> Duration + Send + Sync>;

struct PreparedCodexRuntimeCredential {
    account: ProviderAccount,
    credential: CodexRuntimeCredential,
}

fn access_token_is_current(account: &ProviderAccount, now: SystemTime) -> bool {
    account
        .access_token_expires_at()
        .is_none_or(|expires_at| expires_at > now)
}
