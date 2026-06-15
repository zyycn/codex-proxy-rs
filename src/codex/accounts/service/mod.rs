pub mod cookies;
pub mod health;
pub mod import;
pub mod lifecycle;
pub mod pool_sync;
pub mod quota;
pub mod refresh;

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    codex::accounts::cookies::repository::CookieRepository,
    codex::accounts::{
        model::AccountStatus,
        pool::AccountPool,
        repository::{
            AccountRepository, AccountUsageRepository, StoredAccount, StoredAccountMetadata,
        },
    },
    codex::gateway::{
        fingerprint::model::Fingerprint, oauth::TokenRefresher,
        transport::websocket::CodexWebSocketPool,
    },
    config::AppConfig,
    utils::pagination::Page,
};

#[derive(Clone)]
pub struct AccountService {
    config: Arc<AppConfig>,
    repository: Option<AccountRepository>,
    usage_repository: Option<AccountUsageRepository>,
    cookie_repository: Option<CookieRepository>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    account_pool: Arc<Mutex<AccountPool>>,
    websocket_pool: Arc<CodexWebSocketPool>,
    fingerprint: Fingerprint,
}

pub struct AccountServiceDependencies {
    pub repository: Option<AccountRepository>,
    pub usage_repository: Option<AccountUsageRepository>,
    pub cookie_repository: Option<CookieRepository>,
    pub token_refresher: Option<Arc<dyn TokenRefresher>>,
    pub account_pool: Arc<Mutex<AccountPool>>,
    pub websocket_pool: Arc<CodexWebSocketPool>,
    pub fingerprint: Fingerprint,
}

#[derive(Debug)]
pub enum AccountServiceError {
    RepositoryUnavailable,
    UsageRepositoryUnavailable,
    CookieRepositoryUnavailable,
    AccountNotFound,
    List,
    Export,
    Inspect,
    ResetUsage,
    UpdateLabel,
    UpdateStatus,
    SyncStatus,
    Delete,
    LoadCookies,
    StoreCookies,
    DeleteCookies,
    NoValidCookies,
    EmptyIds,
    InvalidStatus(String),
    LabelTooLong,
    QuotaWarnings,
}

#[derive(Debug)]
pub struct BatchDeleteAccounts {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug)]
pub struct BatchUpdateAccountStatus {
    pub updated: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug)]
pub struct UpdateAccountStatus {
    pub id: String,
    pub status: AccountStatus,
}

#[derive(Debug)]
pub struct AccountImportCounts {
    pub imported: u32,
    pub skipped: u32,
}

#[derive(Debug, Clone)]
pub struct AccountImportEntry {
    pub id: Option<String>,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub token: Option<String>,
    pub refresh_token: Option<String>,
    pub access_token_expires_at: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug)]
pub enum AccountProbeOutcome {
    Alive,
    Dead,
    Skipped,
}

impl AccountProbeOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Dead => "dead",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug)]
pub struct AccountProbeResult {
    pub id: String,
    pub email: Option<String>,
    pub previous_status: AccountStatus,
    pub outcome: AccountProbeOutcome,
    pub status: Option<AccountStatus>,
    pub error: Option<String>,
    pub duration_ms: Option<u128>,
}

#[derive(Debug)]
pub struct AccountQuotaResult {
    pub quota: Value,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountQuotaWarnings {
    pub warnings: Vec<AccountQuotaWarning>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountQuotaWarning {
    pub account_id: String,
    pub email: Option<String>,
    pub window: QuotaWarningWindow,
    pub level: QuotaWarningLevel,
    pub used_percent: f64,
    pub reset_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaWarningWindow {
    Primary,
    Secondary,
}

impl QuotaWarningWindow {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaWarningLevel {
    Warning,
    Critical,
}

impl QuotaWarningLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug)]
pub enum AccountQuotaError {
    RepositoryUnavailable,
    Load,
    NotFound,
    Inactive(AccountStatus),
    StoreQuota,
    Fetch(String),
}

#[derive(Debug)]
pub enum HealthCheckError {
    RepositoryUnavailable,
    List,
}

#[derive(Debug)]
pub enum RefreshAccountError {
    RepositoryUnavailable,
    Load,
    NotFound,
    LeaseAcquire,
    TokenRefresherUnavailable,
    StoreRefreshed,
}

impl std::fmt::Display for RefreshAccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RepositoryUnavailable => write!(f, "repository unavailable"),
            Self::Load => write!(f, "failed to load account"),
            Self::NotFound => write!(f, "account not found"),
            Self::LeaseAcquire => write!(f, "failed to acquire refresh lease"),
            Self::TokenRefresherUnavailable => write!(f, "token refresher unavailable"),
            Self::StoreRefreshed => write!(f, "failed to store refreshed tokens"),
        }
    }
}

#[derive(Debug)]
pub enum StoreImportAccountError {
    RepositoryUnavailable,
    Inspect,
    Invalid(String),
    Insert,
}

#[derive(Debug)]
pub enum ValidatedAccountImportError {
    RepositoryUnavailable,
    TokenRequired,
    TokenRefresherUnavailable,
    RefreshTransport,
    RefreshRejected,
    InvalidToken(&'static str),
    Inspect,
    NotFound,
    Update,
    Insert,
    Load,
}

impl AccountService {
    pub fn new(config: Arc<AppConfig>, dependencies: AccountServiceDependencies) -> Self {
        let AccountServiceDependencies {
            repository,
            usage_repository,
            cookie_repository,
            token_refresher,
            account_pool,
            websocket_pool,
            fingerprint,
        } = dependencies;
        Self {
            config,
            repository,
            usage_repository,
            cookie_repository,
            token_refresher,
            account_pool,
            websocket_pool,
            fingerprint,
        }
    }

    pub fn has_repository(&self) -> bool {
        self.repository.is_some()
    }

    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<StoredAccountMetadata>, AccountServiceError> {
        self.repository()?
            .list_metadata(cursor, limit)
            .await
            .map_err(|_| AccountServiceError::List)
    }

    pub async fn export(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<StoredAccount>, AccountServiceError> {
        let repo = self.repository()?;
        if ids.is_empty() {
            return repo
                .list_all()
                .await
                .map_err(|_| AccountServiceError::Export);
        }

        let mut accounts = Vec::with_capacity(ids.len());
        for id in ids {
            match repo.get(&id).await {
                Ok(Some(account)) => accounts.push(account),
                Ok(None) => {}
                Err(_) => return Err(AccountServiceError::Export),
            }
        }
        Ok(accounts)
    }

    fn repository(&self) -> Result<&AccountRepository, AccountServiceError> {
        self.repository
            .as_ref()
            .ok_or(AccountServiceError::RepositoryUnavailable)
    }

    fn usage_repository(&self) -> Result<&AccountUsageRepository, AccountServiceError> {
        self.usage_repository
            .as_ref()
            .ok_or(AccountServiceError::UsageRepositoryUnavailable)
    }

    fn cookie_repository(&self) -> Result<&CookieRepository, AccountServiceError> {
        self.cookie_repository
            .as_ref()
            .ok_or(AccountServiceError::CookieRepositoryUnavailable)
    }

    async fn ensure_account_exists(&self, account_id: &str) -> Result<(), AccountServiceError> {
        match self.repository()?.exists(account_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(AccountServiceError::AccountNotFound),
            Err(_) => Err(AccountServiceError::Inspect),
        }
    }

    /// 列出所有账户用于刷新调度器
    pub async fn list_all_for_refresh(&self) -> Result<Vec<StoredAccount>, AccountServiceError> {
        self.repository()?
            .list_all()
            .await
            .map_err(|_| AccountServiceError::List)
    }

    /// 列出所有配额锁定的账户ID（用于主动配额刷新）
    pub async fn list_quota_locked_accounts(&self) -> Vec<String> {
        let pool = self.account_pool.lock().await;
        pool.list_quota_locked_accounts()
    }
}
