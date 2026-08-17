use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwnerBinding, PendingAuthorizationMutation,
};
use gateway_admin::model::{MutationActor, MutationContext};
use gateway_core::engine::credential::{AccountStatus, ProviderAccountId};
use gateway_core::routing::ProviderKind;
use provider_openai::OpenAiConfig;
use provider_openai::credential::token_client::{
    AuthorizationCodeExchangeError, AuthorizationCodeExchanger, AuthorizationCodeGrant,
    AuthorizationTokenSet,
};
use provider_openai::credential::{
    CodexCredentialAdmin, CodexCredentialCodec, CodexOAuthAdmin, CodexOAuthAdminError,
    CodexOAuthAdminService, CodexOAuthPendingClaimOutcome, CodexOAuthPendingStore,
    CodexOAuthPendingStoreError, CodexOAuthSecret, CompleteCodexOAuthAuthorization,
    CompletedCodexOAuthCredential, ImportCodexOAuthCredential, StartCodexOAuthAuthorization,
    StoredCodexPendingAuthorization,
};
use provider_openai::transport::profile::CodexWireProfileState;
use secrecy::SecretString;
use url::Url;
use uuid::Uuid;

use crate::support::{MemoryAccountStore, profile as account_profile, secret};

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
            installation_id: pending.installation_id().to_owned(),
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

fn reauthorization_mutation(
    account_id: ProviderAccountId,
    request_id: &str,
) -> PendingAuthorizationMutation {
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: request_id.to_owned(),
    };
    PendingAuthorizationMutation::new(
        ProviderKind::new("openai").expect("provider"),
        AuthorizationMutationTarget::Reauthorize { account_id },
        AuthorizationOwnerBinding::from_context(&context),
    )
}

fn id_token(payload: serde_json::Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("payload JSON"));
    // 官方逻辑只读取 payload；header/signature 不参与本地 metadata 解析。
    format!("unverified-header.{payload}.unverified-signature")
}

async fn complete(
    id_token: String,
) -> Result<(CompletedCodexOAuthCredential, String, String), CodexOAuthAdminError> {
    let pending_store = Arc::new(PendingStore::default());
    let service = CodexOAuthAdminService::new(
        pending_store.clone(),
        Arc::new(Exchanger { id_token }),
        Arc::new(MemoryAccountStore::default()),
        CodexCredentialAdmin,
        profile(),
    );
    let started = service
        .start_authorization(StartCodexOAuthAuthorization {
            mutation: mutation(),
        })
        .await
        .expect("start OAuth authorization");
    let outer = Url::parse(&started.authorization_url).expect("authorization URL");
    let inner_url = outer
        .query_pairs()
        .find_map(|(key, value)| (key == "authorize_url").then(|| value.into_owned()))
        .expect("inner authorization URL");
    let inner = Url::parse(&inner_url).expect("authorization URL");
    let parameters = inner.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
    let state = parameters.get("state").expect("state parameter");
    let surface_stable_id = parameters
        .get("source_surface_stable_id")
        .expect("surface stable ID")
        .to_owned();
    let installation_id = pending_store
        .pending
        .lock()
        .expect("pending lock")
        .as_ref()
        .expect("stored pending authorization")
        .installation_id
        .clone();
    let completed = service
        .complete_authorization(CompleteCodexOAuthAuthorization {
            owner_ref: "test-owner".to_owned(),
            flow_id: started.flow_id,
            callback_url: SecretString::from(format!(
                "http://localhost:1455/auth/callback?code=code-from-browser&state={state}"
            )),
        })
        .await?;
    Ok((completed.credential, installation_id, surface_stable_id))
}

#[tokio::test]
async fn authorize_url_matches_the_official_desktop_parameter_contract() {
    let service = CodexOAuthAdminService::new(
        Arc::new(PendingStore::default()),
        Arc::new(Exchanger {
            id_token: "unused".to_owned(),
        }),
        Arc::new(MemoryAccountStore::default()),
        CodexCredentialAdmin,
        profile(),
    );
    let started = service
        .start_authorization(StartCodexOAuthAuthorization {
            mutation: mutation(),
        })
        .await
        .expect("start OAuth authorization");
    let url = Url::parse(&started.authorization_url).expect("authorization URL");
    let outer_parameters = url.query_pairs().into_owned().collect::<Vec<_>>();
    assert_eq!(
        outer_parameters
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "authorize_url",
            "codex_streamlined_login",
            "no_universal_links"
        ]
    );
    let outer_parameters = outer_parameters.into_iter().collect::<BTreeMap<_, _>>();

    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("chatgpt.com"));
    assert_eq!(url.path(), "/codex/desktop-auth");
    assert_eq!(outer_parameters.len(), 3);
    assert_eq!(
        outer_parameters
            .get("codex_streamlined_login")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        outer_parameters
            .get("no_universal_links")
            .map(String::as_str),
        Some("1")
    );
    let inner = Url::parse(
        outer_parameters
            .get("authorize_url")
            .expect("authorize_url parameter"),
    )
    .expect("inner authorization URL");
    let ordered_parameters = inner.query_pairs().into_owned().collect::<Vec<_>>();
    assert_eq!(
        ordered_parameters
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "response_type",
            "client_id",
            "redirect_uri",
            "scope",
            "code_challenge",
            "code_challenge_method",
            "id_token_add_organizations",
            "codex_cli_simplified_flow",
            "state",
            "originator",
            "codex_app_version",
            "source_surface_stable_id",
            "codex_origin_stable_id",
            "codex_streamlined_login",
        ]
    );
    let parameters = ordered_parameters.into_iter().collect::<BTreeMap<_, _>>();

    assert_eq!(inner.host_str(), Some("auth.openai.com"));
    assert_eq!(inner.path(), "/oauth/authorize");
    assert_eq!(parameters.len(), 14);
    assert!(
        inner
            .query()
            .is_some_and(|query| query.contains("scope=openid+profile+email+offline_access"))
    );
    assert!(
        !inner
            .query()
            .is_some_and(|query| query.contains("scope=openid%20profile"))
    );
    assert_eq!(
        parameters.get("response_type").map(String::as_str),
        Some("code")
    );
    assert_eq!(
        parameters.get("redirect_uri").map(String::as_str),
        Some("http://localhost:1455/auth/callback")
    );
    assert_eq!(
        parameters.get("scope").map(String::as_str),
        Some("openid profile email offline_access api.connectors.read api.connectors.invoke")
    );
    assert_eq!(
        parameters.get("originator").map(String::as_str),
        Some("Codex Desktop")
    );
    assert_eq!(
        parameters.get("codex_app_version").map(String::as_str),
        Some("26.803.81509")
    );
    let surface_stable_id = parameters
        .get("source_surface_stable_id")
        .expect("surface stable ID");
    let parsed_surface_id = Uuid::parse_str(surface_stable_id).expect("UUID surface stable ID");
    assert_eq!(parsed_surface_id.get_version_num(), 4);
    assert_eq!(
        parameters.get("codex_origin_stable_id"),
        Some(surface_stable_id)
    );
    assert_eq!(
        parameters
            .get("codex_streamlined_login")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        parameters.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        parameters
            .get("id_token_add_organizations")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        parameters
            .get("codex_cli_simplified_flow")
            .map(String::as_str),
        Some("true")
    );
    assert!(
        parameters
            .get("client_id")
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        parameters
            .get("state")
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        parameters
            .get("code_challenge")
            .is_some_and(|value| !value.is_empty())
    );
    assert!(!parameters.contains_key("nonce"));
}

fn profile() -> CodexWireProfileState {
    OpenAiConfig::default().wire_profile_state()
}

#[tokio::test]
async fn separate_new_authorizations_use_distinct_surface_stable_ids() {
    let mut ids = Vec::new();
    for _ in 0..2 {
        let service = CodexOAuthAdminService::new(
            Arc::new(PendingStore::default()),
            Arc::new(Exchanger {
                id_token: "unused".to_owned(),
            }),
            Arc::new(MemoryAccountStore::default()),
            CodexCredentialAdmin,
            profile(),
        );
        let started = service
            .start_authorization(StartCodexOAuthAuthorization {
                mutation: mutation(),
            })
            .await
            .expect("start OAuth authorization");
        let outer = Url::parse(&started.authorization_url).expect("outer authorization URL");
        let inner = outer
            .query_pairs()
            .find_map(|(key, value)| (key == "authorize_url").then(|| value.into_owned()))
            .and_then(|value| Url::parse(&value).ok())
            .expect("inner authorization URL");
        let surface_stable_id = inner
            .query_pairs()
            .find_map(|(key, value)| {
                (key == "source_surface_stable_id").then(|| value.into_owned())
            })
            .expect("surface stable ID");
        ids.push(surface_stable_id);
    }
    assert_ne!(ids[0], ids[1]);
}

#[tokio::test]
async fn repeated_reauthorization_derives_the_same_surface_id_for_one_account() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_surface_reauth".to_owned(),
            name: "surface reauthorization".to_owned(),
            secret: secret("surface-reauth-access"),
            verified_account: account_profile("chatgpt-surface-reauth"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account_id = ProviderAccountId::new("acct_surface_reauth").expect("account id");
    let mut surface_ids = Vec::new();
    for request_id in ["surface-reauth-first", "surface-reauth-second"] {
        let service = CodexOAuthAdminService::new(
            Arc::new(PendingStore::default()),
            Arc::new(Exchanger {
                id_token: "unused".to_owned(),
            }),
            store.clone(),
            CodexCredentialAdmin,
            profile(),
        );
        let started = service
            .start_authorization(StartCodexOAuthAuthorization {
                mutation: reauthorization_mutation(account_id.clone(), request_id),
            })
            .await
            .expect("start OAuth reauthorization");
        let outer = Url::parse(&started.authorization_url).expect("outer authorization URL");
        let inner = outer
            .query_pairs()
            .find_map(|(key, value)| (key == "authorize_url").then(|| value.into_owned()))
            .and_then(|value| Url::parse(&value).ok())
            .expect("inner authorization URL");
        surface_ids.push(
            inner
                .query_pairs()
                .find_map(|(key, value)| {
                    (key == "source_surface_stable_id").then(|| value.into_owned())
                })
                .expect("surface stable ID"),
        );
    }

    assert_eq!(surface_ids[0], surface_ids[1]);
}

#[tokio::test]
async fn first_exchange_persists_the_installation_id_without_reusing_it_as_the_surface_id() {
    let (credential, pending_installation_id, surface_stable_id) = complete(id_token(
        serde_json::json!({ "email": "identity@example.com" }),
    ))
    .await
    .expect("completed OAuth exchange");
    let CompletedCodexOAuthCredential::Create(account) = credential else {
        panic!("expected account creation");
    };
    let runtime = CodexCredentialCodec::decode(&account.credential).expect("stored credential");

    assert_eq!(runtime.installation_id, pending_installation_id);
    assert_ne!(runtime.installation_id, surface_stable_id);
}

#[tokio::test]
async fn first_exchange_uses_official_id_token_claim_mapping_without_signature_validation() {
    let (credential, _, _) = complete(id_token(serde_json::json!({
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
    let (credential, _, _) = complete(id_token(serde_json::json!({
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
