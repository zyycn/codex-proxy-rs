use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwnerBinding, PendingAuthorizationMutation,
};
use gateway_admin::model::{MutationActor, MutationContext};
use gateway_core::engine::credential::ProviderAccountId;
use gateway_core::routing::ProviderKind;
use provider_openai::credential::token_client::{
    AuthorizationCodeExchangeError, AuthorizationCodeExchanger, AuthorizationCodeGrant,
    AuthorizationTokenSet,
};
use provider_openai::credential::{
    CodexAccountIdentityVerifier, CodexCredentialAdmin, CodexIdentityExpectation,
    CodexIdentityVerification, CodexIdentityVerificationError, CodexOAuthAdmin,
    CodexOAuthAdminError, CodexOAuthAdminService, CodexOAuthPendingClaimOutcome,
    CodexOAuthPendingStore, CodexOAuthPendingStoreError, CodexOAuthSecret,
    CodexPendingAuthorization, CompleteCodexOAuthAuthorization, CompletedCodexOAuthCredential,
    ImportCodexOAuthCredential, StartCodexOAuthAuthorization, StoredCodexPendingAuthorization,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest as _, Sha256};
use url::Url;

use crate::support::{MemoryAccountStore, profile, runtime_policy, secret};

#[derive(Default)]
struct PendingStore {
    value: Mutex<Option<(StoredCodexPendingAuthorization, Option<String>)>>,
}

fn duplicate_pending(
    pending: &StoredCodexPendingAuthorization,
) -> Result<CodexPendingAuthorization, CodexOAuthPendingStoreError> {
    CodexPendingAuthorization::from_stored(StoredCodexPendingAuthorization {
        flow_id: pending.flow_id.clone(),
        owner_ref: pending.owner_ref.clone(),
        started_request_ref: pending.started_request_ref.clone(),
        name: pending.name.clone(),
        expires_at: pending.expires_at,
        state: pending.state.clone(),
        nonce: pending.nonce.clone(),
        code_verifier: pending.code_verifier.clone(),
        reauthorization_account_id: pending.reauthorization_account_id.clone(),
        mutation: pending.mutation.clone(),
    })
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
        *value = Some((
            StoredCodexPendingAuthorization {
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
            },
            None,
        ));
        Ok(())
    }

    async fn claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
        _claim_ttl: std::time::Duration,
    ) -> Result<CodexOAuthPendingClaimOutcome, CodexOAuthPendingStoreError> {
        let mut value = self.value.lock().expect("pending lock");
        let Some((pending, claim)) = value.as_mut() else {
            return Ok(CodexOAuthPendingClaimOutcome::NotFound);
        };
        if pending.owner_ref != owner_ref || pending.flow_id != flow_id {
            return Ok(CodexOAuthPendingClaimOutcome::NotFound);
        }
        if claim.is_some() {
            return Ok(CodexOAuthPendingClaimOutcome::InProgress);
        }
        *claim = Some(claim_ref.to_owned());
        duplicate_pending(pending)
            .map(Box::new)
            .map(CodexOAuthPendingClaimOutcome::Claimed)
    }

    async fn release_claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
    ) -> Result<bool, CodexOAuthPendingStoreError> {
        let mut value = self.value.lock().expect("pending lock");
        let Some((pending, claim)) = value.as_mut() else {
            return Ok(false);
        };
        if pending.owner_ref != owner_ref
            || pending.flow_id != flow_id
            || claim.as_deref() != Some(claim_ref)
        {
            return Ok(false);
        }
        *claim = None;
        Ok(true)
    }

    async fn consume_claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
    ) -> Result<bool, CodexOAuthPendingStoreError> {
        let mut value = self.value.lock().expect("pending lock");
        let Some((pending, claim)) = value.as_ref() else {
            return Ok(false);
        };
        if pending.owner_ref != owner_ref
            || pending.flow_id != flow_id
            || claim.as_deref() != Some(claim_ref)
        {
            return Ok(false);
        }
        *value = None;
        Ok(true)
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
        ProviderKind::new("openai").expect("provider"),
        AuthorizationMutationTarget::Create {
            name: "OAuth Account".to_owned(),
        },
        AuthorizationOwnerBinding::from_context(&context),
    )
}

fn reauthorization_mutation(request_id: &str, account_id: &str) -> PendingAuthorizationMutation {
    let context = owner_context(request_id);
    PendingAuthorizationMutation::new(
        ProviderKind::new("openai").expect("provider"),
        AuthorizationMutationTarget::Reauthorize {
            account_id: ProviderAccountId::new(account_id).expect("account id"),
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
async fn completion_accepts_unrelated_oauth_callback_parameters() {
    let service = service();
    let started = started(&service).await;
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
                "http://localhost:1455/auth/callback?code=authorization-code&state={state}&scope=openid%20profile&iss=https%3A%2F%2Fauth.openai.com"
            )),
        })
        .await
        .expect("complete OAuth with standard callback parameters");

    assert!(matches!(
        prepared.credential,
        CompletedCodexOAuthCredential::Create(_)
    ));
}

#[tokio::test]
async fn completion_rejects_repeated_code_callback_parameter() {
    let service = service();
    let started = started(&service).await;
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
                "http://localhost:1455/auth/callback?code=authorization-code&code=another-code&state={state}"
            )),
        })
        .await
        .expect_err("repeated code");

    assert_eq!(error, CodexOAuthAdminError::CallbackRejected);
}

#[tokio::test]
async fn completion_rejects_repeated_state_callback_parameter() {
    let service = service();
    let started = started(&service).await;
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
                "http://localhost:1455/auth/callback?code=authorization-code&state={state}&state=another-state-value"
            )),
        })
        .await
        .expect_err("repeated state");

    assert_eq!(error, CodexOAuthAdminError::CallbackRejected);
}

struct UnavailableVerifier;

#[async_trait]
impl CodexAccountIdentityVerifier for UnavailableVerifier {
    async fn verify(
        &self,
        _: &CodexOAuthSecret,
        _: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Unavailable)
    }

    async fn verify_authorization(
        &self,
        _: &CodexOAuthSecret,
        _: &SecretString,
        _: &SecretString,
        _: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Unavailable)
    }
}

#[tokio::test]
async fn completion_classifies_unavailable_identity_verification_as_upstream_unavailable() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service_with_store(store, Arc::new(UnavailableVerifier));
    let started = started(&service).await;
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
        .expect_err("identity verification unavailable");

    assert_eq!(error, CodexOAuthAdminError::UpstreamUnavailable);
}

#[tokio::test]
async fn callback_state_mismatch_releases_flow_for_retry() {
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
        CodexOAuthAdminError::CallbackRejected
    );
    assert_eq!(
        service
            .complete_authorization(command())
            .await
            .expect_err("retryable callback rejection"),
        CodexOAuthAdminError::CallbackRejected
    );
}

#[tokio::test]
async fn token_exchange_rejection_releases_flow_for_retry() {
    let service = service();
    let started = started(&service).await;
    let flow_id = started.flow_id;
    let state = Url::parse(&started.authorization_url)
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .expect("state");
    let command = || CompleteCodexOAuthAuthorization {
        owner_ref: owner_ref(),
        flow_id: flow_id.clone(),
        callback_url: SecretString::from(format!(
            "http://localhost:1455/auth/callback?code=rejected-authorization-code&state={state}"
        )),
    };

    assert_eq!(
        service
            .complete_authorization(command())
            .await
            .expect_err("token exchange rejection"),
        CodexOAuthAdminError::TokenRejected
    );
    assert_eq!(
        service
            .complete_authorization(command())
            .await
            .expect_err("retryable token exchange rejection"),
        CodexOAuthAdminError::TokenRejected
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
async fn reauthorization_uses_revision_advanced_after_start_for_prepared_rotation() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_oauth_reauth".to_owned(),
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
    let advanced_revision = store.advance_credential_revision("acct_oauth_reauth").await;
    assert_eq!(advanced_revision.get(), 2);
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
    assert_eq!(prepared.credential.expected_revision().get(), 2);
    let runtime =
        provider_openai::credential::CodexCredentialCodec::decode(prepared.credential.credential())
            .expect("prepared credential");
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .expect("OAuth credential")
            .access_token
            .expose_secret(),
        "oauth-access-token"
    );
    assert_eq!(runtime.installation_id, original_installation_id);
    assert_eq!(
        store
            .account("acct_oauth_reauth")
            .expect("unchanged account")
            .revision()
            .get(),
        2
    );
}

struct ReplacementIdentityVerifier;

#[async_trait]
impl CodexAccountIdentityVerifier for ReplacementIdentityVerifier {
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
        expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        assert_eq!(expectation, &CodexIdentityExpectation::default());
        Ok(CodexIdentityVerification::Complete(profile(
            "chatgpt-different-owner",
        )))
    }
}

struct RotatedPrincipalVerifier;

#[async_trait]
impl CodexAccountIdentityVerifier for RotatedPrincipalVerifier {
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
        let mut verified = profile("chatgpt-oauth");
        verified.oauth_subject = "rotated-oauth-subject".to_owned();
        verified.poid = None;
        Ok(CodexIdentityVerification::Complete(verified))
    }
}

#[tokio::test]
async fn reauthorization_updates_changed_free_principal_for_same_account() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_oauth_principal".to_owned(),
            name: "reauthorize".to_owned(),
            secret: secret("old-oauth-access"),
            verified_account: profile("chatgpt-oauth"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let service = service_with_store(store, Arc::new(RotatedPrincipalVerifier));
    let started = service
        .start_authorization(StartCodexOAuthAuthorization {
            mutation: reauthorization_mutation("request-rotated-principal", "acct_oauth_principal"),
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
    let runtime =
        provider_openai::credential::CodexCredentialCodec::decode(prepared.credential.credential())
            .expect("prepared credential");
    let principal = runtime.principal.expect("OAuth principal");

    assert_eq!(principal.oauth_subject, "rotated-oauth-subject");
    assert_eq!(principal.poid, None);
}

#[tokio::test]
async fn reauthorization_rebinds_target_record_to_authorized_identity() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_oauth_rebind".to_owned(),
            name: "reauthorize".to_owned(),
            secret: secret("old-oauth-access"),
            verified_account: profile("chatgpt-oauth"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let service = service_with_store(store, Arc::new(ReplacementIdentityVerifier));
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
    let completed = service
        .complete_authorization(CompleteCodexOAuthAuthorization {
            owner_ref: owner_ref(),
            flow_id: started.flow_id,
            callback_url: SecretString::from(format!(
                "http://localhost:1455/auth/callback?code=authorization-code&state={state}"
            )),
        })
        .await
        .expect("complete identity replacement");
    let CompletedCodexOAuthCredential::Reauthorize(prepared) = completed.credential else {
        panic!("expected prepared reauthorization");
    };
    assert_eq!(
        prepared.credential.account_id().as_str(),
        "acct_oauth_rebind"
    );
    let replacement = prepared
        .replacement_identity
        .as_ref()
        .expect("replacement identity");
    assert_eq!(
        replacement.upstream_account_id(),
        Some("chatgpt-different-owner")
    );
    assert_eq!(
        replacement.upstream_user_id(),
        "user-chatgpt-different-owner"
    );
}
