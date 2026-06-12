pub mod cookies;
pub mod health;
pub mod import;
pub mod mutation;
pub mod quota;
pub mod refresh;
pub mod runtime_pool;

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    codex::accounts::{
        model::AccountStatus,
        pool::AccountPool,
        repository::{
            AccountRepository, AccountUsageRepository, StoredAccount, StoredAccountMetadata,
        },
    },
    codex::cookies::repository::CookieRepository,
    codex::oauth::TokenRefresher,
    config::AppConfig,
    utils::pagination::Page,
};

#[derive(Clone)]
pub struct AccountService {
    config: AppConfig,
    repository: Option<AccountRepository>,
    usage_repository: Option<AccountUsageRepository>,
    cookie_repository: Option<CookieRepository>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    account_pool: Arc<Mutex<AccountPool>>,
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
    TokenRefresherUnavailable,
    StoreRefreshed,
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
    pub fn new(
        config: AppConfig,
        repository: Option<AccountRepository>,
        usage_repository: Option<AccountUsageRepository>,
        cookie_repository: Option<CookieRepository>,
        token_refresher: Option<Arc<dyn TokenRefresher>>,
        account_pool: Arc<Mutex<AccountPool>>,
    ) -> Self {
        Self {
            config,
            repository,
            usage_repository,
            cookie_repository,
            token_refresher,
            account_pool,
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
}
