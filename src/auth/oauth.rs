#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub auth_endpoint: String,
    pub token_endpoint: String,
}
