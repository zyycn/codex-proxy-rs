use thiserror::Error;

use crate::{
    auth::{api_key::ApiKeyHasher, error::AuthError},
    codex::accounts::repository::AccountRepositoryError,
    codex::oauth::OpenAiOAuthRefresher,
    config::AppConfig,
    storage::db::connect_sqlite,
    utils::crypto::{CryptoError, SecretBox},
};

use super::state::AppState;

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

pub async fn build_state(config: AppConfig) -> BootstrapResult<(AppState, usize)> {
    let secret_box = SecretBox::load_or_create(&config.security.master_key_file)?;
    let api_key_hasher = ApiKeyHasher::load_or_create(&config.security.api_key_pepper_file)?;
    let oauth_client = OpenAiOAuthRefresher::codex_default(
        reqwest::Client::builder()
            .use_rustls_tls()
            .no_proxy()
            .build()?,
    );
    let pool = connect_sqlite(&config.database.url).await?;
    let state = AppState::with_pool_secret_api_key_hasher_and_oauth_client(
        config,
        pool,
        secret_box,
        api_key_hasher,
        oauth_client,
    );
    let restored_accounts = state.reload_account_pool_from_repository().await?;
    Ok((state, restored_accounts))
}
