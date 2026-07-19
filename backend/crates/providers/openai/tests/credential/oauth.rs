use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use provider_openai::credential::token_client::{
    AuthorizationCodeExchangeError, AuthorizationCodeExchanger, AuthorizationCodeGrant,
    AuthorizationTokenSet,
};
use provider_openai::credential::{
    CodexAccountProfile, CodexAuthorizationTokenVerifier, CodexCredentialAdmin,
    CodexIdentityVerificationError, CodexOAuthAdmin, CodexOAuthAdminError, CodexOAuthAdminService,
    CodexOAuthFlowBinding, CodexOAuthPendingStore, CodexOAuthPendingStoreError, CodexOAuthSecret,
    CodexPendingAuthorization, CompleteCodexOAuthAuthorization, CreateCodexCredential,
    StartCodexOAuthAuthorization, StartCodexOAuthReauthorization, StoredCodexPendingAuthorization,
};
use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::support::{MemoryAccountStore, instance_id, profile, secret};

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
impl CodexAuthorizationTokenVerifier for Verifier {
    async fn verify_authorization(
        &self,
        _secret: &CodexOAuthSecret,
        id_token: &SecretString,
        expected_nonce: &SecretString,
    ) -> Result<CodexAccountProfile, CodexIdentityVerificationError> {
        if id_token.expose_secret() != "oauth-id-token" || expected_nonce.expose_secret().len() < 16
        {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        Ok(profile("chatgpt-oauth"))
    }
}

fn service() -> CodexOAuthAdminService {
    service_with_store(Arc::new(MemoryAccountStore::default()), Arc::new(Verifier))
}

fn service_with_store(
    store: Arc<MemoryAccountStore>,
    verifier: Arc<dyn CodexAuthorizationTokenVerifier>,
) -> CodexOAuthAdminService {
    CodexOAuthAdminService::new(
        Arc::new(PendingStore::default()),
        Arc::new(Exchanger),
        verifier,
        store,
        CodexCredentialAdmin,
    )
}

async fn started(
    service: &CodexOAuthAdminService,
) -> provider_openai::credential::CodexOAuthAuthorizationStarted {
    service
        .start_authorization(StartCodexOAuthAuthorization {
            binding: CodexOAuthFlowBinding::new("owner-one", "request-one").expect("binding"),
            provider_instance_id: "inst_openai_primary".to_owned(),
            name: "OAuth Account".to_owned(),
        })
        .await
        .expect("start OAuth")
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
            owner_ref: "owner-one".to_owned(),
            flow_id: started.flow_id,
            callback_url: SecretString::from(format!(
                "http://localhost:1455/auth/callback?code=authorization-code&state={state}"
            )),
        })
        .await
        .expect("complete OAuth");
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
        owner_ref: "owner-one".to_owned(),
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
        .repository()
        .create_oauth_credential(CreateCodexCredential {
            account_id: "acct_oauth_reauth".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "reauthorize".to_owned(),
            secret: secret("old-oauth-access"),
            account: profile("chatgpt-oauth"),
            enabled: true,
        })
        .await
        .expect("create account");
    let service = service_with_store(store.clone(), Arc::new(Verifier));
    let started = service
        .start_reauthorization(StartCodexOAuthReauthorization {
            binding: CodexOAuthFlowBinding::new("owner-one", "request-reauth").expect("binding"),
            account_id: gateway_core::engine::credential::ProviderAccountId::new(
                "acct_oauth_reauth",
            )
            .expect("account id"),
            expected_credential_revision:
                gateway_core::engine::credential::CredentialRevision::new(1).expect("revision"),
        })
        .await
        .expect("start reauthorization");
    let state = Url::parse(&started.authorization_url)
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .expect("state");
    let prepared = service
        .complete_reauthorization(CompleteCodexOAuthAuthorization {
            owner_ref: "owner-one".to_owned(),
            flow_id: started.flow_id,
            callback_url: SecretString::from(format!(
                "http://localhost:1455/auth/callback?code=authorization-code&state={state}"
            )),
        })
        .await
        .expect("complete reauthorization");

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
impl CodexAuthorizationTokenVerifier for RebindingVerifier {
    async fn verify_authorization(
        &self,
        _: &CodexOAuthSecret,
        _: &SecretString,
        _: &SecretString,
    ) -> Result<CodexAccountProfile, CodexIdentityVerificationError> {
        Ok(profile("chatgpt-different-owner"))
    }
}

#[tokio::test]
async fn reauthorization_rejects_identity_rebinding() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .repository()
        .create_oauth_credential(CreateCodexCredential {
            account_id: "acct_oauth_rebind".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "reauthorize".to_owned(),
            secret: secret("old-oauth-access"),
            account: profile("chatgpt-oauth"),
            enabled: true,
        })
        .await
        .expect("create account");
    let service = service_with_store(store, Arc::new(RebindingVerifier));
    let started = service
        .start_reauthorization(StartCodexOAuthReauthorization {
            binding: CodexOAuthFlowBinding::new("owner-one", "request-rebind").expect("binding"),
            account_id: gateway_core::engine::credential::ProviderAccountId::new(
                "acct_oauth_rebind",
            )
            .expect("account id"),
            expected_credential_revision:
                gateway_core::engine::credential::CredentialRevision::new(1).expect("revision"),
        })
        .await
        .expect("start reauthorization");
    let state = Url::parse(&started.authorization_url)
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .expect("state");
    let error = service
        .complete_reauthorization(CompleteCodexOAuthAuthorization {
            owner_ref: "owner-one".to_owned(),
            flow_id: started.flow_id,
            callback_url: SecretString::from(format!(
                "http://localhost:1455/auth/callback?code=authorization-code&state={state}"
            )),
        })
        .await
        .expect_err("identity rebind");
    assert_eq!(error, CodexOAuthAdminError::Credential);
}
