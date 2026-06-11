use codex_proxy_rs::{
    app::build_router,
    auth::{api_key::ApiKeyHasher, oauth::OpenAiOAuthRefresher},
    config::AppConfig,
    crypto::SecretBox,
    logs::rotation::{init_tracing, RotationConfig},
    state::AppState,
    storage::db::connect_sqlite,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::load()?;

    let _log_writer = init_tracing(RotationConfig::new(
        &config.logging.directory,
        config.logging.max_file_bytes,
        config.logging.retention_days,
    ))?;
    let host = config.server.host.clone();
    let port = config.server.port;
    let secret_box = SecretBox::load_or_create(&config.security.master_key_file)?;
    let api_key_hasher = ApiKeyHasher::load_or_create(&config.security.api_key_pepper_file)?;
    let token_refresher = OpenAiOAuthRefresher::codex_default(
        reqwest::Client::builder()
            .use_rustls_tls()
            .no_proxy()
            .build()?,
    );
    let pool = connect_sqlite(&config.database.url).await?;
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        config,
        pool,
        secret_box,
        api_key_hasher,
        token_refresher,
    );
    let restored_accounts = state.reload_account_pool_from_repository().await?;
    tracing::info!(restored_accounts, "account pool restored from sqlite");
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    tracing::info!(host, port, "codex-proxy-rs listening");
    axum::serve(listener, app).await?;
    Ok(())
}
