use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{TimeZone as _, Utc};
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
            "accessToken": " ",
            "refreshToken": "",
            "idToken": "not.a.parseable.jwt"
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
    assert!(account.account.access_token_expires_at().is_none());
    assert!(account.account.next_refresh_at().is_none());
}

#[tokio::test]
async fn direct_import_accepts_snake_case_oauth_token_aliases() {
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accounts": [{
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "access_token": "snake-access-token",
                    "refresh_token": "snake-refresh-token",
                    "id_token": "snake-id-token"
                }
            }]
        }))
        .await
        .expect("snake_case OAuth token aliases must be accepted");

    let account = prepared.accounts().first().expect("one prepared account");
    let runtime = CodexCredentialCodec::decode(&account.credential).expect("stored credential");
    let oauth = runtime.authentication.oauth().expect("OAuth credential");
    assert_eq!(
        (
            oauth.access_token.expose_secret(),
            oauth
                .refresh_token
                .as_ref()
                .map(|token| token.expose_secret()),
            oauth.id_token.as_ref().map(|token| token.expose_secret()),
        ),
        (
            "snake-access-token",
            Some("snake-refresh-token"),
            Some("snake-id-token"),
        )
    );
}

#[tokio::test]
async fn direct_import_projects_access_token_jwt_expiry_without_persisting_refresh_margin() {
    let expires_at = Utc
        .timestamp_opt(2_000_000_000, 0)
        .single()
        .expect("valid test timestamp");
    let access_token = format!(
        "unverified-header.{}.unverified-signature",
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({"exp": expires_at.timestamp()}))
                .expect("test JWT payload"),
        )
    );
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": access_token,
            "refreshToken": "refresh-token"
        }))
        .await
        .expect("direct JWT import must be accepted");

    let account = prepared.accounts().first().expect("one prepared account");
    assert_eq!(
        account
            .account
            .access_token_expires_at()
            .map(chrono::DateTime::<Utc>::from),
        Some(expires_at)
    );
    assert!(account.account.next_refresh_at().is_none());
}

fn test_jwt(payload: serde_json::Value) -> String {
    format!(
        "unverified-header.{}.unverified-signature",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("test JWT payload"))
    )
}

#[tokio::test]
async fn oauth_import_uses_official_chatgpt_access_token_claims() {
    let access_token = test_jwt(serde_json::json!({
        "email": "top-level@example.com",
        "https://api.openai.com/profile": {"email": "profile@example.com"},
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_user_id": "chatgpt-user",
            "user_id": "fallback-user",
            "chatgpt_account_id": "chatgpt-account"
        }
    }));
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": access_token
        }))
        .await
        .expect("official access token claims are accepted");

    let account = &prepared.accounts().first().expect("one account").account;
    assert_eq!(account.upstream_user_id(), Some("chatgpt-user"));
    assert_eq!(account.upstream_account_id(), Some("chatgpt-account"));
    assert_eq!(account.email(), Some("top-level@example.com"));
    assert_eq!(account.plan_type(), Some("pro"));
}

#[tokio::test]
async fn oauth_import_uses_official_plan_alias_projection_and_user_id_fallback() {
    let access_token = test_jwt(serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "hc",
            "user_id": "fallback-user",
            "chatgpt_account_is_fedramp": false
        }
    }));
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": access_token
        }))
        .await
        .expect("official plan alias and user ID fallback are accepted");

    let account = &prepared.accounts().first().expect("one account").account;
    assert_eq!(account.upstream_user_id(), Some("fallback-user"));
    assert_eq!(account.plan_type(), Some("enterprise"));
}

#[tokio::test]
async fn oauth_import_uses_id_token_then_access_token_for_missing_claims() {
    let id_token = test_jwt(serde_json::json!({
        "https://api.openai.com/profile": {"email": "id-token@example.com"},
        "https://api.openai.com/auth": {"chatgpt_user_id": "id-token-user"}
    }));
    let access_token = test_jwt(serde_json::json!({
        "email": "access-token@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "plus",
            "chatgpt_user_id": "access-token-user",
            "chatgpt_account_id": "access-token-account"
        }
    }));
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": access_token,
            "idToken": id_token
        }))
        .await
        .expect("ID token and access token claims are accepted");

    let account = &prepared.accounts().first().expect("one account").account;
    assert_eq!(account.upstream_user_id(), Some("id-token-user"));
    assert_eq!(account.upstream_account_id(), Some("access-token-account"));
    assert_eq!(account.email(), Some("id-token@example.com"));
    assert_eq!(account.plan_type(), Some("plus"));
}

#[tokio::test]
async fn oauth_import_does_not_use_top_level_identity_fields() {
    let access_token = test_jwt(serde_json::json!({"exp": 2_000_000_000_i64}));
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": access_token,
            "userId": "untrusted-user",
            "accountId": "untrusted-account",
            "email": "untrusted@example.com",
            "planType": "pro"
        }))
        .await
        .expect("token without identity claims remains importable");

    let account = &prepared.accounts().first().expect("one account").account;
    assert!(account.upstream_user_id().is_none());
    assert!(account.upstream_account_id().is_none());
    assert!(account.email().is_none());
    assert!(account.plan_type().is_none());
}

#[tokio::test]
async fn oauth_import_rejects_legacy_bare_token_field() {
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let error = service
        .prepare_import_document(serde_json::json!({
            "token": "header.payload.signature"
        }))
        .await
        .expect_err("ambiguous token field must not be accepted");

    assert_eq!(
        error,
        provider_openai::credential::CodexCredentialAdminError::InvalidCredential
    );
}
