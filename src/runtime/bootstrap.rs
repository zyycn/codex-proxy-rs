use thiserror::Error;

use crate::{
    admin::session::repository::AdminAuthRepository,
    codex::accounts::repository::AccountRepositoryError,
    codex::gateway::fingerprint::{model::Fingerprint, repository::FingerprintRepository},
    codex::gateway::oauth::OpenAiOAuthRefresher,
    codex::serving::dispatch::affinity::SessionAffinityRepositoryError,
    config::AppConfig,
    platform::{
        crypto::{CryptoError, SecretBox},
        identity::{
            admin_session::hash_admin_password, client_key::ApiKeyHasher, error::AuthError,
        },
        storage::db::connect_sqlite,
    },
};

use super::state::AppState;
use sqlx::SqlitePool;

#[derive(Debug, Error)]
pub enum BootstrapError {
    #[error("secret storage error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("api key hasher error: {0}")]
    Auth(#[from] AuthError),
    #[error("http client error: {0}")]
    HttpClient(#[from] reqwest::Error),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("account repository error: {0}")]
    AccountRepository(#[from] AccountRepositoryError),
    #[error("session affinity repository error: {0}")]
    SessionAffinityRepository(#[from] SessionAffinityRepositoryError),
}

pub type BootstrapResult<T> = Result<T, BootstrapError>;

pub async fn build_state(config: AppConfig) -> BootstrapResult<(AppState, SqlitePool, usize)> {
    let secret_box = SecretBox::load_or_create(&config.security.master_key_file)?;
    let api_key_hasher = ApiKeyHasher::load_or_create(&config.security.api_key_pepper_file)?;
    let oauth_client = OpenAiOAuthRefresher::codex_default(
        reqwest::Client::builder()
            .use_rustls_tls()
            .no_proxy()
            .build()?,
    );
    let pool = connect_sqlite(&config.database.url).await?;

    // 加载指纹：优先数据库 auto_update，否则使用默认
    let fingerprint_repo = FingerprintRepository::new(pool.clone());
    let fingerprint = match fingerprint_repo.load_latest_auto_updated().await {
        Ok(Some(fp)) => {
            tracing::info!(
                version = %fp.app_version,
                build = %fp.build_number,
                source = "database",
                "已从数据库加载请求 fingerprint"
            );
            fp
        }
        Ok(None) => {
            let fp = Fingerprint::default_codex_desktop();
            tracing::info!(
                version = %fp.app_version,
                build = %fp.build_number,
                source = "default",
                "使用默认请求 fingerprint"
            );
            fp
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "从数据库加载 fingerprint 失败，使用默认值"
            );
            Fingerprint::default_codex_desktop()
        }
    };

    let admin_auth_repo = AdminAuthRepository::new(pool.clone());
    ensure_default_admin_exists(&admin_auth_repo, &config).await?;

    let state = AppState::with_pool_secret_api_key_hasher_oauth_client_and_fingerprint(
        config,
        pool.clone(),
        secret_box,
        api_key_hasher,
        oauth_client,
        fingerprint,
    );
    let restored_accounts = state.reload_account_pool_from_repository().await?;
    let restored_affinities = state.reload_session_affinity_from_repository().await?;
    tracing::info!(
        account_count = restored_accounts,
        session_affinity_count = restored_affinities,
        "已从 SQLite 恢复运行时状态"
    );
    Ok((state, pool, restored_accounts))
}

/// 确保默认管理员账号存在（首次启动时创建）
async fn ensure_default_admin_exists(
    repository: &AdminAuthRepository,
    config: &AppConfig,
) -> Result<(), sqlx::Error> {
    let password_hash = hash_admin_password(&config.admin.default_password)
        .map_err(|e| sqlx::Error::Protocol(format!("failed to hash password: {e}")))?;
    if repository.ensure_default_admin(&password_hash).await? {
        tracing::info!(
            username = %config.admin.default_username,
            "已创建默认管理员用户"
        );
    }

    Ok(())
}
