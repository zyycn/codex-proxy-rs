use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwnerBinding, PendingAuthorizationMutation,
};
use gateway_admin::model::{MutationActor, MutationContext};
use gateway_core::engine::credential::AccountStatus;
use gateway_core::routing::ProviderKind;
use provider_openai::credential::token_client::{
    AuthorizationCodeExchangeError, AuthorizationCodeExchanger, AuthorizationCodeGrant,
    AuthorizationTokenSet,
};
use provider_openai::credential::{
    CodexCredentialAdmin, CodexOAuthAdmin, CodexOAuthAdminError, CodexOAuthAdminService,
    CodexOAuthPendingClaimOutcome, CodexOAuthPendingStore, CodexOAuthPendingStoreError,
    CodexOAuthSecret, CompleteCodexOAuthAuthorization, CompletedCodexOAuthCredential,
    StartCodexOAuthAuthorization, StoredCodexPendingAuthorization,
};
use secrecy::SecretString;
use url::Url;

use crate::support::MemoryAccountStore;

#[derive(Default)]
struct PendingStore {
    pending: Mutex<Option<StoredCodexPendingAuthorization>>,
}

#[async_trait]
impl CodexOAuthPendingStore for PendingStore {
    async fn create(
        &self,
        pending: &provider_openai::credential::CodexPendingAuthorization,
    ) -> Result<(), CodexOAuthPendingStoreError> {
        let mut stored = self.pending.lock().expect("pending lock");
        if stored.is_some() {
            return Err(CodexOAuthPendingStoreError::Conflict);
        }
        *stored = Some(StoredCodexPendingAuthorization {
            flow_id: pending.flow_id().to_owned(),
            owner_ref: pending.owner_ref().to_owned(),
            started_request_ref: pending.started_request_ref().to_owned(),
            name: pending.name().to_owned(),
            expires_at: pending.expires_at(),
            state: pending.state().clone(),
            nonce: pending.nonce().clone(),
            code_verifier: pending.code_verifier().clone(),
            reauthorization_account_id: pending.reauthorization().map(ToString::to_string),
            mutation: pending.mutation().clone(),
        });
        Ok(())
    }

    async fn claim(
        &self,
        _owner_ref: &str,
        _flow_id: &str,
        _claim_ref: &str,
        _claim_ttl: Duration,
    ) -> Result<CodexOAuthPendingClaimOutcome, CodexOAuthPendingStoreError> {
        let Some(pending) = self.pending.lock().expect("pending lock").take() else {
            return Ok(CodexOAuthPendingClaimOutcome::NotFound);
        };
        let pending = provider_openai::credential::CodexPendingAuthorization::from_stored(pending)
            .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)?;
        Ok(CodexOAuthPendingClaimOutcome::Claimed(Box::new(pending)))
    }

    async fn release_claim(
        &self,
        _owner_ref: &str,
        _flow_id: &str,
        _claim_ref: &str,
    ) -> Result<bool, CodexOAuthPendingStoreError> {
        Ok(true)
    }

    async fn consume_claim(
        &self,
        _owner_ref: &str,
        _flow_id: &str,
        _claim_ref: &str,
    ) -> Result<bool, CodexOAuthPendingStoreError> {
        self.pending.lock().expect("pending lock").take();
        Ok(true)
    }
}

struct Exchanger {
    id_token: String,
}

#[async_trait]
impl AuthorizationCodeExchanger for Exchanger {
    async fn exchange_authorization_code(
        &self,
        _grant: AuthorizationCodeGrant,
    ) -> Result<AuthorizationTokenSet, AuthorizationCodeExchangeError> {
        Ok(AuthorizationTokenSet {
            secret: CodexOAuthSecret {
                access_token: SecretString::from("access-token-from-upstream"),
                refresh_token: Some(SecretString::from("refresh-token-from-upstream")),
                id_token: None,
            },
            id_token: SecretString::from(self.id_token.clone()),
            expires_in: Some(Duration::from_secs(60 * 60)),
        })
    }
}

fn mutation() -> PendingAuthorizationMutation {
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "oauth-alignment-test".to_owned(),
    };
    PendingAuthorizationMutation::new(
        ProviderKind::new("openai").expect("provider"),
        AuthorizationMutationTarget::Create {
            name: "new OAuth account".to_owned(),
        },
        AuthorizationOwnerBinding::from_context(&context),
    )
}

fn id_token(payload: serde_json::Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("payload JSON"));
    // 官方逻辑只读取 payload；header/signature 不参与本地 metadata 解析。
    format!("unverified-header.{payload}.unverified-signature")
}

async fn complete(id_token: String) -> Result<CompletedCodexOAuthCredential, CodexOAuthAdminError> {
    let service = CodexOAuthAdminService::new(
        Arc::new(PendingStore::default()),
        Arc::new(Exchanger { id_token }),
        Arc::new(MemoryAccountStore::default()),
        CodexCredentialAdmin,
    );
    let started = service
        .start_authorization(StartCodexOAuthAuthorization {
            mutation: mutation(),
        })
        .await
        .expect("start OAuth authorization");
    let state = Url::parse(&started.authorization_url)
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .expect("state parameter");
    let completed = service
        .complete_authorization(CompleteCodexOAuthAuthorization {
            owner_ref: "test-owner".to_owned(),
            flow_id: started.flow_id,
            callback_url: SecretString::from(format!(
                "http://localhost:1455/auth/callback?code=code-from-browser&state={state}"
            )),
        })
        .await?;
    Ok(completed.credential)
}

#[tokio::test]
async fn first_exchange_uses_official_id_token_claim_mapping_without_signature_validation() {
    let credential = complete(id_token(serde_json::json!({
        "email": "top@example.com",
        "https://api.openai.com/profile": { "email": "fallback@example.com" },
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_user_id": "chatgpt-user",
            "user_id": "fallback-user",
            "chatgpt_account_id": "workspace-id"
        }
    })))
    .await
    .expect("completed OAuth exchange");

    let CompletedCodexOAuthCredential::Create(account) = credential else {
        panic!("expected account creation");
    };
    assert_eq!(account.account.email(), Some("top@example.com"));
    assert_eq!(account.account.upstream_user_id(), Some("chatgpt-user"));
    assert_eq!(account.account.upstream_account_id(), Some("workspace-id"));
    assert_eq!(account.account.plan_type(), Some("pro"));
    assert_eq!(
        account
            .account
            .status_projection(SystemTime::now(), None)
            .status,
        AccountStatus::Normal
    );
    assert!(
        account.account.next_refresh_at().is_none(),
        "normal OAuth creation must not persist a margin-derived refresh time"
    );
}

#[tokio::test]
async fn first_exchange_permits_missing_identity_claims_but_requires_a_parseable_id_token() {
    let credential = complete(id_token(serde_json::json!({
        "https://api.openai.com/profile": { "email": "profile@example.com" }
    })))
    .await
    .expect("missing optional claims are allowed");
    let CompletedCodexOAuthCredential::Create(account) = credential else {
        panic!("expected account creation");
    };
    assert_eq!(account.account.email(), Some("profile@example.com"));
    assert_eq!(account.account.upstream_user_id(), None);
    assert_eq!(
        account
            .account
            .status_projection(SystemTime::now(), None)
            .status,
        AccountStatus::Error
    );

    let error = complete("header.not-base64.signature".to_owned())
        .await
        .expect_err("official local payload parsing rejects malformed id tokens");
    assert_eq!(error, CodexOAuthAdminError::TokenRejected);
}
