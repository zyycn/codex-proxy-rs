use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sqlx::{Row, SqlitePool};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    auth::admin_session::verify_admin_password,
    codex::accounts::service::{AccountService, ValidatedAccountImportError},
    codex::accounts::{
        model::AccountStatus,
        pool::AccountPool,
        repository::{AccountRepository, StoredAccountMetadata},
    },
    codex::oauth::{
        DeviceCode, OAuthClient, OAuthConfig, OAuthError, PkceSession, PkceSessionStore,
    },
    config::AppConfig,
};

#[derive(Clone)]
pub struct AdminAuthService {
    config: AppConfig,
    db: Option<SqlitePool>,
    account_repository: Option<AccountRepository>,
    account_pool: Arc<Mutex<AccountPool>>,
    oauth_client: Option<Arc<dyn OAuthClient>>,
    oauth_sessions: Arc<Mutex<PkceSessionStore>>,
    accounts: AccountService,
}

#[derive(Debug)]
pub enum AdminAuthServiceError {
    DatabaseUnavailable,
    LoadAdminUser,
    AdminPasswordInvalid,
    VerifyAdminPassword,
    InvalidSessionTtl,
    CreateSession,
    AccountRepositoryUnavailable,
    InspectAccountAuthStatus,
    ClearAccounts,
}

#[derive(Debug)]
pub enum AdminSessionValidationError {
    DatabaseUnavailable,
    ValidateSession,
}

#[derive(Debug)]
pub enum AdminAuthOAuthError {
    OAuthClientUnavailable,
    AccountRepositoryUnavailable,
    DeviceCodeRequired,
    InvalidOrExpiredSession,
    DeviceCodeRequest(OAuthError),
    DeviceAuthorization(OAuthError),
    TokenExchange(OAuthError),
    Import(ValidatedAccountImportError),
}

#[derive(Debug)]
pub struct AdminLogin {
    pub session_id: String,
    pub expires_at: DateTime<Utc>,
    pub ttl_minutes: u64,
}

#[derive(Debug)]
pub struct AdminAuthStatus {
    pub authenticated: bool,
    pub user: Option<AdminAuthUser>,
    pub pool: AdminAuthPoolSummary,
}

#[derive(Debug)]
pub struct AdminAuthUser {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub status: AccountStatus,
    pub access_token_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default)]
pub struct AdminAuthPoolSummary {
    pub total: usize,
    pub active: usize,
    pub expired: usize,
    pub quota_exhausted: usize,
    pub refreshing: usize,
    pub disabled: usize,
    pub banned: usize,
}

#[derive(Debug)]
pub struct AdminLogout {
    pub deleted: u64,
}

#[derive(Debug)]
pub struct AdminAuthLoginStart {
    pub auth_url: String,
    pub state: String,
}

#[derive(Debug)]
pub struct AdminAuthDevicePoll {
    pub success: bool,
    pub pending: bool,
    pub code: Option<String>,
}

#[derive(Debug)]
pub enum AdminAuthPkceExchange {
    Imported { return_host: String },
    AlreadyCompleted,
}

#[derive(Debug)]
struct AdminUserRow {
    id: String,
    password_hash: String,
}

impl AdminAuthService {
    pub fn new(
        config: AppConfig,
        db: Option<SqlitePool>,
        account_repository: Option<AccountRepository>,
        account_pool: Arc<Mutex<AccountPool>>,
        oauth_client: Option<Arc<dyn OAuthClient>>,
        oauth_sessions: Arc<Mutex<PkceSessionStore>>,
        accounts: AccountService,
    ) -> Self {
        Self {
            config,
            db,
            account_repository,
            account_pool,
            oauth_client,
            oauth_sessions,
            accounts,
        }
    }

    pub async fn login(&self, password: &str) -> Result<AdminLogin, AdminAuthServiceError> {
        let pool = self.pool()?;
        let admin = load_first_admin(pool)
            .await
            .map_err(|_| AdminAuthServiceError::LoadAdminUser)?
            .ok_or(AdminAuthServiceError::AdminPasswordInvalid)?;

        match verify_admin_password(password, &admin.password_hash) {
            Ok(true) => {}
            Ok(false) => return Err(AdminAuthServiceError::AdminPasswordInvalid),
            Err(_) => return Err(AdminAuthServiceError::VerifyAdminPassword),
        }

        let ttl_minutes = self.config.admin.session_ttl_minutes;
        let ttl_minutes_i64 =
            i64::try_from(ttl_minutes).map_err(|_| AdminAuthServiceError::InvalidSessionTtl)?;
        let expires_at = Utc::now() + Duration::minutes(ttl_minutes_i64);
        let session_id = format!("sess_{}", Uuid::new_v4().simple());
        create_admin_session(pool, &session_id, &admin.id, expires_at)
            .await
            .map_err(|_| AdminAuthServiceError::CreateSession)?;

        Ok(AdminLogin {
            session_id,
            expires_at,
            ttl_minutes,
        })
    }

    pub async fn validate_session(
        &self,
        session_id: Option<&str>,
    ) -> Result<bool, AdminSessionValidationError> {
        let Some(session_id) = session_id else {
            return Ok(false);
        };
        let pool = self
            .pool()
            .map_err(|_| AdminSessionValidationError::DatabaseUnavailable)?;
        validate_admin_session(pool, session_id)
            .await
            .map_err(|_| AdminSessionValidationError::ValidateSession)
    }

    pub async fn status(&self) -> Result<AdminAuthStatus, AdminAuthServiceError> {
        let repo = self.account_repository()?;
        let accounts = repo
            .list_all_metadata()
            .await
            .map_err(|_| AdminAuthServiceError::InspectAccountAuthStatus)?;
        let pool = account_auth_pool_summary(&accounts);
        let user = accounts.first().map(account_auth_user);
        Ok(AdminAuthStatus {
            authenticated: pool.total > 0,
            user,
            pool,
        })
    }

    pub async fn logout(&self) -> Result<AdminLogout, AdminAuthServiceError> {
        let repo = self.account_repository()?;
        let deleted = repo
            .delete_all()
            .await
            .map_err(|_| AdminAuthServiceError::ClearAccounts)?;
        self.account_pool.lock().await.clear();
        Ok(AdminLogout { deleted })
    }

    pub async fn request_device_code(&self) -> Result<DeviceCode, AdminAuthOAuthError> {
        self.oauth_client()?
            .request_device_code()
            .await
            .map_err(AdminAuthOAuthError::DeviceCodeRequest)
    }

    pub async fn poll_device_token(
        &self,
        device_code: &str,
    ) -> Result<AdminAuthDevicePoll, AdminAuthOAuthError> {
        let oauth_client = self.oauth_client()?;
        if !self.accounts.has_repository() {
            return Err(AdminAuthOAuthError::AccountRepositoryUnavailable);
        }
        let device_code = device_code.trim();
        if device_code.is_empty() {
            return Err(AdminAuthOAuthError::DeviceCodeRequired);
        }

        match oauth_client.poll_device_token(device_code).await {
            Ok(tokens) => {
                self.accounts
                    .import_validated(Some(tokens.access_token), tokens.refresh_token)
                    .await
                    .map_err(AdminAuthOAuthError::Import)?;
                Ok(AdminAuthDevicePoll {
                    success: true,
                    pending: false,
                    code: None,
                })
            }
            Err(error) => {
                if let Some(code) = error.pending_code() {
                    return Ok(AdminAuthDevicePoll {
                        success: false,
                        pending: true,
                        code: Some(code.to_string()),
                    });
                }
                Err(AdminAuthOAuthError::DeviceAuthorization(error))
            }
        }
    }

    pub async fn start_pkce_login(&self, return_host: &str) -> AdminAuthLoginStart {
        let login = self
            .oauth_sessions
            .lock()
            .await
            .start_login(return_host, &OAuthConfig::codex_default());
        AdminAuthLoginStart {
            auth_url: login.auth_url,
            state: login.state,
        }
    }

    pub async fn exchange_pkce_code(
        &self,
        oauth_state: &str,
        code: &str,
    ) -> Result<AdminAuthPkceExchange, AdminAuthOAuthError> {
        if !self.accounts.has_repository() {
            return Err(AdminAuthOAuthError::AccountRepositoryUnavailable);
        }
        let oauth_client = self.oauth_client()?;
        let Some(session) = self.acquire_pkce_session(oauth_state).await? else {
            return Ok(AdminAuthPkceExchange::AlreadyCompleted);
        };
        self.exchange_pkce_session(oauth_client.as_ref(), oauth_state, code, session)
            .await
    }

    fn pool(&self) -> Result<&SqlitePool, AdminAuthServiceError> {
        self.db
            .as_ref()
            .ok_or(AdminAuthServiceError::DatabaseUnavailable)
    }

    fn account_repository(&self) -> Result<&AccountRepository, AdminAuthServiceError> {
        self.account_repository
            .as_ref()
            .ok_or(AdminAuthServiceError::AccountRepositoryUnavailable)
    }

    fn oauth_client(&self) -> Result<Arc<dyn OAuthClient>, AdminAuthOAuthError> {
        self.oauth_client
            .clone()
            .ok_or(AdminAuthOAuthError::OAuthClientUnavailable)
    }

    async fn acquire_pkce_session(
        &self,
        oauth_state: &str,
    ) -> Result<Option<PkceSession>, AdminAuthOAuthError> {
        let mut sessions = self.oauth_sessions.lock().await;
        if let Some(session) = sessions.try_acquire(oauth_state) {
            return Ok(Some(session));
        }
        if sessions.is_completed_or_exchanging(oauth_state) {
            return Ok(None);
        }
        Err(AdminAuthOAuthError::InvalidOrExpiredSession)
    }

    async fn exchange_pkce_session(
        &self,
        oauth_client: &dyn OAuthClient,
        oauth_state: &str,
        code: &str,
        session: PkceSession,
    ) -> Result<AdminAuthPkceExchange, AdminAuthOAuthError> {
        let tokens = match oauth_client
            .exchange_code(code, &session.code_verifier, &session.redirect_uri)
            .await
        {
            Ok(tokens) => tokens,
            Err(error) => {
                self.oauth_sessions.lock().await.release(oauth_state);
                return Err(AdminAuthOAuthError::TokenExchange(error));
            }
        };
        match self
            .accounts
            .import_validated(Some(tokens.access_token), tokens.refresh_token)
            .await
        {
            Ok(_) => {
                self.oauth_sessions.lock().await.complete(oauth_state);
                Ok(AdminAuthPkceExchange::Imported {
                    return_host: session.return_host,
                })
            }
            Err(error) => {
                self.oauth_sessions.lock().await.complete(oauth_state);
                Err(AdminAuthOAuthError::Import(error))
            }
        }
    }
}

async fn load_first_admin(pool: &SqlitePool) -> Result<Option<AdminUserRow>, sqlx::Error> {
    let row =
        sqlx::query("select id, password_hash from admin_users order by created_at asc limit 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|row| AdminUserRow {
        id: row.get("id"),
        password_hash: row.get("password_hash"),
    }))
}

async fn validate_admin_session(pool: &SqlitePool, session_id: &str) -> Result<bool, sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    let count: (i64,) =
        sqlx::query_as("select count(*) from admin_sessions where id = ? and expires_at > ?")
            .bind(session_id)
            .bind(now)
            .fetch_one(pool)
            .await?;
    Ok(count.0 > 0)
}

async fn create_admin_session(
    pool: &SqlitePool,
    session_id: &str,
    user_id: &str,
    expires_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(expires_at.to_rfc3339())
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

fn account_auth_user(account: &StoredAccountMetadata) -> AdminAuthUser {
    AdminAuthUser {
        id: account.id.clone(),
        email: account.email.clone(),
        account_id: account.account_id.clone(),
        user_id: account.user_id.clone(),
        label: account.label.clone(),
        plan_type: account.plan_type.clone(),
        status: account.status,
        access_token_expires_at: account.access_token_expires_at,
    }
}

fn account_auth_pool_summary(accounts: &[StoredAccountMetadata]) -> AdminAuthPoolSummary {
    let mut summary = AdminAuthPoolSummary {
        total: accounts.len(),
        ..AdminAuthPoolSummary::default()
    };
    for account in accounts {
        match account.status {
            AccountStatus::Active => summary.active += 1,
            AccountStatus::Expired => summary.expired += 1,
            AccountStatus::QuotaExhausted => summary.quota_exhausted += 1,
            AccountStatus::Refreshing => summary.refreshing += 1,
            AccountStatus::Disabled => summary.disabled += 1,
            AccountStatus::Banned => summary.banned += 1,
        }
    }
    summary
}
