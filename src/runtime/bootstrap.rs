use thiserror::Error;

use crate::{
    codex::accounts::repository::AccountRepositoryError,
    codex::gateway::fingerprint::{model::Fingerprint, repository::FingerprintRepository},
    codex::gateway::oauth::OpenAiOAuthRefresher,
    config::AppConfig,
    platform::{
        crypto::{CryptoError, SecretBox},
        identity::{admin_session::hash_admin_password, api_key::ApiKeyHasher, error::AuthError},
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
                "loaded fingerprint for requests"
            );
            fp
        }
        Ok(None) => {
            let fp = Fingerprint::default_codex_desktop();
            tracing::info!(
                version = %fp.app_version,
                build = %fp.build_number,
                source = "default",
                "using default fingerprint for requests"
            );
            fp
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "failed to load fingerprint from database, using default"
            );
            Fingerprint::default_codex_desktop()
        }
    };

    // 初始化默认管理员账号（如果不存在）
    ensure_default_admin_exists(&pool, &config).await?;

    let state = AppState::with_pool_secret_api_key_hasher_oauth_client_and_fingerprint(
        config,
        pool.clone(),
        secret_box,
        api_key_hasher,
        oauth_client,
        fingerprint,
    );
    let restored_accounts = state.reload_account_pool_from_repository().await?;
    Ok((state, pool, restored_accounts))
}

/// 确保默认管理员账号存在（首次启动时创建）
async fn ensure_default_admin_exists(
    pool: &SqlitePool,
    config: &AppConfig,
) -> Result<(), sqlx::Error> {
    // 检查是否已存在管理员
    let count: (i64,) = sqlx::query_as("select count(*) from admin_users")
        .fetch_one(pool)
        .await?;

    if count.0 == 0 {
        // 创建默认管理员
        let admin_id = format!("admin_{}", uuid::Uuid::new_v4().simple());
        let password_hash = hash_admin_password(&config.admin.default_password)
            .map_err(|e| sqlx::Error::Protocol(format!("failed to hash password: {}", e)))?;
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
        )
        .bind(&admin_id)
        .bind(&password_hash)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await?;

        tracing::info!(
            username = %config.admin.default_username,
            "created default admin user"
        );
    }

    Ok(())
}
