use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    codex_proxy_rs::runtime::bootstrap::run().await
}
