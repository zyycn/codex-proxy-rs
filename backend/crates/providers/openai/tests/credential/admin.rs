use std::sync::Arc;

use async_trait::async_trait;
use provider_openai::credential::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use provider_openai::credential::{CodexCredentialAdminService, CodexCredentialCodec};
use secrecy::ExposeSecret as _;

use crate::support::{TestLeaseCoordinator, runtime_policy};

struct UnusedRefresher;

#[async_trait]
impl TokenRefresher for UnusedRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        panic!("a direct access-token import must not refresh its refresh token")
    }
}

#[tokio::test]
async fn direct_import_persists_opaque_tokens_without_profile_or_token_validation() {
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "token": " ",
            "refresh_token": "",
            "id_token": "not.a.parseable.jwt"
        }))
        .await
        .expect("opaque direct import must be accepted");

    let account = prepared.accounts().first().expect("one prepared account");
    let runtime = CodexCredentialCodec::decode(&account.credential).expect("stored credential");
    let oauth = runtime.authentication.oauth().expect("OAuth credential");
    assert_eq!(oauth.access_token.expose_secret(), " ");
    assert_eq!(
        oauth
            .refresh_token
            .as_ref()
            .expect("provided refresh token")
            .expose_secret(),
        ""
    );
    assert_eq!(
        oauth
            .id_token
            .as_ref()
            .expect("provided ID token")
            .expose_secret(),
        "not.a.parseable.jwt"
    );
    assert!(account.account.upstream_user_id().is_none());
}
