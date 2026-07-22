use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwnerBinding, PendingAuthorizationMutation,
};
use gateway_admin::model::{MutationActor, MutationContext, Revision};
use gateway_core::engine::credential::ProviderAccountId;
use gateway_core::routing::ProviderKind;
use provider_openai::credential::token_client::{
    AuthorizationCodeExchangeError, AuthorizationCodeExchanger, AuthorizationCodeGrant,
    AuthorizationTokenSet,
};
use provider_openai::credential::{
    CodexAccountIdentityVerifier, CodexCredentialAdmin, CodexIdentityExpectation,
    CodexIdentityVerification, CodexIdentityVerificationError, CodexOAuthAdmin,
    CodexOAuthAdminError, CodexOAuthAdminService, CodexOAuthPendingStore,
    CodexOAuthPendingStoreError, CodexOAuthSecret, CodexPendingAuthorization,
    CompleteCodexOAuthAuthorization, CompletedCodexOAuthCredential, ImportCodexOAuthCredential,
    StartCodexOAuthAuthorization, StoredCodexPendingAuthorization,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest as _, Sha256};
use url::Url;

use crate::support::{MemoryAccountStore, instance_id, profile, runtime_policy, secret};

#[derive(Default)]
struct PendingStore {
    value: Mutex<Option<StoredCodexPendingAuthorization>>,
}

#[async_trait]
impl CodexOAuthPendingStore for PendingStore {
    async fn create(
        &self,
        pending: &CodexPendingAuthorization,
    ) -> Result<(), CodexOAuthPendingStoreError> {
        let mut value = self.value.lock().expect("pending lock");
        if value.is_some() {
            return Err(CodexOAuthPendingStoreError::Conflict);
        }
        *value = Some(StoredCodexPendingAuthorization {
            flow_id: pending.flow_id().to_owned(),
            owner_ref: pending.owner_ref().to_owned(),
            started_request_ref: pending.started_request_ref().to_owned(),
            provider_instance_id: pending.provider_instance_id().to_owned(),
            name: pending.name().to_owned(),
            expires_at: pending.expires_at(),
            state: pending.state().clone(),
            nonce: pending.nonce().clone(),
            code_verifier: pending.code_verifier().clone(),
            reauthorization_account_id: pending
                .reauthorization()
                .map(|target| target.account_id().to_string()),
            reauthorization_credential_revision: pending
                .reauthorization()
                .map(|target| target.credential_revision().get()),
            mutation: pending.mutation().clone(),
        });
        Ok(())
    }

    async fn take(
        &self,
        owner_ref: &str,
        flow_id: &str,
    ) -> Result<Option<CodexPendingAuthorization>, CodexOAuthPendingStoreError> {
        let mut value = self.value.lock().expect("pending lock");
        if value
            .as_ref()
            .is_some_and(|pending| pending.owner_ref == owner_ref && pending.flow_id == flow_id)
        {
            return value
                .take()
                .map(CodexPendingAuthorization::from_stored)
                .transpose();
        }
        Ok(None)
    }
}

struct Exchanger;

#[async_trait]
impl AuthorizationCodeExchanger for Exchanger {
    async fn exchange_authorization_code(
        &self,
        grant: AuthorizationCodeGrant,
    ) -> Result<AuthorizationTokenSet, AuthorizationCodeExchangeError> {
        if grant.code.expose_secret() != "authorization-code" {
            return Err(AuthorizationCodeExchangeError::Rejected);
        }
        Ok(AuthorizationTokenSet {
            secret: CodexOAuthSecret {
                access_token: SecretString::from("oauth-access-token"),
                refresh_token: Some(SecretString::from("oauth-refresh-token")),
                id_token: None,
            },
            id_token: SecretString::from("oauth-id-token"),
        })
    }
}

struct Verifier;

#[async_trait]
impl CodexAccountIdentityVerifier for Verifier {
    async fn verify(
        &self,
        _secret: &CodexOAuthSecret,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Rejected)
    }

    async fn verify_authorization(
        &self,
        _secret: &CodexOAuthSecret,
        id_token: &SecretString,
        expected_nonce: &SecretString,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        if id_token.expose_secret() != "oauth-id-token" || expected_nonce.expose_secret().len() < 16
        {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        Ok(CodexIdentityVerification::Complete(profile(
            "chatgpt-oauth",
        )))
    }
}

fn service() -> CodexOAuthAdminService {
    service_with_store(Arc::new(MemoryAccountStore::default()), Arc::new(Verifier))
}

fn service_with_store(
    store: Arc<MemoryAccountStore>,
    verifier: Arc<dyn CodexAccountIdentityVerifier>,
) -> CodexOAuthAdminService {
    CodexOAuthAdminService::new(
        Arc::new(PendingStore::default()),
        Arc::new(Exchanger),
        verifier,
        store,
        runtime_policy(),
        CodexCredentialAdmin,
    )
}

async fn started(
    service: &CodexOAuthAdminService,
) -> provider_openai::credential::CodexOAuthAuthorizationStarted {
    service
        .start_authorization(StartCodexOAuthAuthorization {
            mutation: create_mutation("request-one"),
        })
        .await
        .expect("start OAuth")
}

fn owner_context(request_id: &str) -> MutationContext {
    MutationContext {
        actor: MutationActor::AdminSession {
            admin_user_id: "owner-one".to_owned(),
        },
        request_id: request_id.to_owned(),
    }
}

fn create_mutation(request_id: &str) -> PendingAuthorizationMutation {
    let context = owner_context(request_id);
    PendingAuthorizationMutation::new(
        Revision::new(1).expect("revision"),
        ProviderKind::new("openai").expect("provider"),
        AuthorizationMutationTarget::Create {
            provider_instance_id: instance_id(),
            name: "OAuth Account".to_owned(),
        },
        AuthorizationOwnerBinding::from_context(&context),
    )
}

fn reauthorization_mutation(request_id: &str, account_id: &str) -> PendingAuthorizationMutation {
    let context = owner_context(request_id);
    PendingAuthorizationMutation::new(
        Revision::new(1).expect("revision"),
        ProviderKind::new("openai").expect("provider"),
        AuthorizationMutationTarget::Reauthorize {
            provider_instance_id: instance_id(),
            account_id: ProviderAccountId::new(account_id).expect("account id"),
            expected_credential_revision: Revision::new(1).expect("revision"),
        },
        AuthorizationOwnerBinding::from_context(&context),
    )
}

fn owner_ref() -> String {
    let mut digest = Sha256::new();
    digest.update(b"admin-session\0");
    digest.update(b"owner-one");
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

#[tokio::test]
async fn start_authorization_uses_pkce_nonce_and_fixed_official_redirect() {
    let service = service();
    let started = started(&service).await;
    let url = Url::parse(&started.authorization_url).expect("authorization URL");
    let query = url
        .query_pairs()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        url.as_str().split('?').next(),
        Some("https://auth.openai.com/oauth/authorize")
    );
    assert_eq!(
        query.get("code_challenge_method").map(|v| v.as_ref()),
        Some("S256")
    );
    assert_eq!(
        query.get("redirect_uri").map(|v| v.as_ref()),
        Some("http://localhost:1455/auth/callback")
    );
    assert!(query.get("state").is_some_and(|value| value.len() >= 16));
    assert!(query.get("nonce").is_some_and(|value| value.len() >= 16));
}

#[tokio::test]
async fn completion_returns_verified_core_account_without_writing_store() {
    let service = service();
    let started = started(&service).await;
    let url = Url::parse(&started.authorization_url).expect("authorization URL");
    let state = url
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .expect("state");
    let prepared = service
        .complete_authorization(CompleteCodexOAuthAuthorization {
            owner_ref: owner_ref(),
            flow_id: started.flow_id,
            callback_url: SecretString::from(format!(
                "http://localhost:1455/auth/callback?code=authorization-code&state={state}"
            )),
        })
        .await
        .expect("complete OAuth");
    let CompletedCodexOAuthCredential::Create(prepared) = prepared.credential else {
        panic!("expected prepared create");
    };
    assert_eq!(prepared.account.provider().as_str(), "openai");
    assert!(prepared.account.id().as_str().starts_with("acct_"));
    assert_eq!(
        prepared
            .credential
            .expose_to_provider()
            .get("access_token")
            .and_then(serde_json::Value::as_str),
        Some("oauth-access-token")
    );
    assert_eq!(
        prepared
            .credential
            .expose_to_provider()
            .get("id_token")
            .and_then(serde_json::Value::as_str),
        Some("oauth-id-token")
    );
}

#[tokio::test]
async fn callback_state_mismatch_fails_closed_and_consumes_flow() {
    let service = service();
    let started = started(&service).await;
    let flow_id = started.flow_id;
    let command = || CompleteCodexOAuthAuthorization {
        owner_ref: owner_ref(),
        flow_id: flow_id.clone(),
        callback_url: SecretString::from(
            "http://localhost:1455/auth/callback?code=authorization-code&state=wrong-state-value",
        ),
    };
    assert_eq!(
        service
            .complete_authorization(command())
            .await
            .expect_err("bad state"),
        CodexOAuthAdminError::UpstreamRejected
    );
    assert_eq!(
        service
            .complete_authorization(command())
            .await
            .expect_err("one shot"),
        CodexOAuthAdminError::NotFound
    );
}

#[test]
fn oauth_command_debug_redacts_owner_flow_and_callback() {
    let command = CompleteCodexOAuthAuthorization {
        owner_ref: "owner-private".to_owned(),
        flow_id: "flow-private".to_owned(),
        callback_url: SecretString::from("callback-private-value"),
    };
    let debug = format!("{command:?}");
    for secret in ["owner-private", "flow-private", "callback-private-value"] {
        assert!(!debug.contains(secret));
    }
}

#[tokio::test]
async fn reauthorization_binds_account_revision_and_returns_only_prepared_rotation() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_oauth_reauth".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "reauthorize".to_owned(),
            secret: secret("old-oauth-access"),
            verified_account: profile("chatgpt-oauth"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let original_account = store
        .account("acct_oauth_reauth")
        .expect("original account");
    let original_installation_id = store
        .repository()
        .load_runtime_credential(&original_account)
        .await
        .expect("original credential")
        .installation_id;
    let service = service_with_store(store.clone(), Arc::new(Verifier));
    let started = service
        .start_authorization(StartCodexOAuthAuthorization {
            mutation: reauthorization_mutation("request-reauth", "acct_oauth_reauth"),
        })
        .await
        .expect("start reauthorization");
    let state = Url::parse(&started.authorization_url)
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .expect("state");
    let prepared = service
        .complete_authorization(CompleteCodexOAuthAuthorization {
            owner_ref: owner_ref(),
            flow_id: started.flow_id,
            callback_url: SecretString::from(format!(
                "http://localhost:1455/auth/callback?code=authorization-code&state={state}"
            )),
        })
        .await
        .expect("complete reauthorization");
    let CompletedCodexOAuthCredential::Reauthorize(prepared) = prepared.credential else {
        panic!("expected prepared reauthorization");
    };

    assert_eq!(
        prepared.credential.account_id().as_str(),
        "acct_oauth_reauth"
    );
    assert_eq!(prepared.credential.expected_revision().get(), 1);
    let runtime =
        provider_openai::credential::CodexCredentialCodec::decode(prepared.credential.credential())
            .expect("prepared credential");
    assert_eq!(
        runtime.secret.access_token.expose_secret(),
        "oauth-access-token"
    );
    assert_eq!(runtime.installation_id, original_installation_id);
    assert_eq!(
        store
            .account("acct_oauth_reauth")
            .expect("unchanged account")
            .revision()
            .get(),
        1
    );
}

struct RebindingVerifier;

#[async_trait]
impl CodexAccountIdentityVerifier for RebindingVerifier {
    async fn verify(
        &self,
        _: &CodexOAuthSecret,
        _: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Rejected)
    }

    async fn verify_authorization(
        &self,
        _: &CodexOAuthSecret,
        _: &SecretString,
        _: &SecretString,
        _: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Ok(CodexIdentityVerification::Complete(profile(
            "chatgpt-different-owner",
        )))
    }
}

#[tokio::test]
async fn reauthorization_rejects_identity_rebinding() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_oauth_rebind".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "reauthorize".to_owned(),
            secret: secret("old-oauth-access"),
            verified_account: profile("chatgpt-oauth"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let service = service_with_store(store, Arc::new(RebindingVerifier));
    let started = service
        .start_authorization(StartCodexOAuthAuthorization {
            mutation: reauthorization_mutation("request-rebind", "acct_oauth_rebind"),
        })
        .await
        .expect("start reauthorization");
    let state = Url::parse(&started.authorization_url)
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .expect("state");
    let error = service
        .complete_authorization(CompleteCodexOAuthAuthorization {
            owner_ref: owner_ref(),
            flow_id: started.flow_id,
            callback_url: SecretString::from(format!(
                "http://localhost:1455/auth/callback?code=authorization-code&state={state}"
            )),
        })
        .await
        .expect_err("identity rebind");
    assert_eq!(error, CodexOAuthAdminError::Credential);
}
