use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwnerBinding, PendingAuthorizationMutation,
};
use gateway_admin::model::{MutationActor, MutationContext};
use gateway_core::account::{
    AccountStatus, CredentialRevision, NewProviderAccount, ProviderAccount, ProviderAccountId,
    ProviderAccountStore,
};
use gateway_core::routing::ProviderKind;
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

/// 走完整的重新授权流程（start → callback → complete），返回最终结果。
async fn complete_reauthorization(
    store: Arc<MemoryAccountStore>,
    account_id: ProviderAccountId,
    id_token: String,
    request_id: &str,
) -> Result<CompletedCodexOAuthCredential, CodexOAuthAdminError> {
    let pending_store = Arc::new(PendingStore::default());
    let service = CodexOAuthAdminService::new(
        pending_store.clone(),
        Arc::new(Exchanger { id_token }),
        store,
        CodexCredentialAdmin,
        profile(),
    );
    let started = service
        .start_authorization(StartCodexOAuthAuthorization {
            mutation: reauthorization_mutation(account_id, request_id),
        })
        .await
        .expect("start OAuth reauthorization");
    let outer = Url::parse(&started.authorization_url).expect("authorization URL");
    let state = outer
        .query_pairs()
        .find_map(|(key, value)| (key == "authorize_url").then(|| value.into_owned()))
        .and_then(|inner| Url::parse(&inner).ok())
        .and_then(|inner| {
            inner
                .query_pairs()
                .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        })
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

/// 手工构造没有上游身份列的已存行，覆盖 `upstream_user_id`/`upstream_account_id` 为 NULL。
async fn seed_account_without_upstream_identity(
    store: &MemoryAccountStore,
    account_id: &str,
    upstream_user_id: Option<&str>,
    upstream_account_id: Option<&str>,
) {
    let id = ProviderAccountId::new(account_id).expect("account id");
    let provider = ProviderKind::new("openai").expect("provider");
    let verified = account_profile(account_id);
    let credential =
        CodexCredentialCodec::encode_new(&secret("manual-identity-access"), &verified, Vec::new())
            .expect("encode credential");
    let account = ProviderAccount::new(
        id,
        provider,
        "manual identity".to_owned(),
        upstream_user_id.map(str::to_owned),
        provider_openai::credential::CODEX_AUTHENTICATION_KIND_OAUTH.to_owned(),
        CredentialRevision::new(1).expect("revision"),
        None,
    )
    .with_profile(
        verified.email.clone(),
        upstream_account_id.map(str::to_owned),
        verified.plan_type.clone(),
    );
    store
        .create_account(NewProviderAccount {
            account,
            credential,
            model_access: None,
        })
        .await
        .expect("seed account without upstream identity");
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
        Some("26.901.51231")
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
    provider_openai::transport::profile::CodexWireProfileState::new(Default::default())
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

/// 重新授权时在授权页选择了另一个 ChatGPT 账号，必须拒绝，并保持身份列与
/// 已存凭据原样（不得出现“身份列属于账号 A、令牌属于账号 B”的半写状态）。
#[tokio::test]
async fn reauthorization_with_a_different_upstream_identity_keeps_old_identity_columns() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_reauth_identity".to_owned(),
            name: "reauthorization identity".to_owned(),
            secret: secret("reauth-identity-access"),
            verified_account: account_profile("chatgpt-reauth-identity"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account_id = ProviderAccountId::new("acct_reauth_identity").expect("account id");
    let before = store
        .load_current_credential(&account_id)
        .await
        .expect("seeded credential");

    let error = complete_reauthorization(
        store.clone(),
        account_id.clone(),
        id_token(serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro",
                "chatgpt_user_id": "chatgpt-other-user",
                "chatgpt_account_id": "other-workspace-id"
            }
        })),
        "reauth-identity-different",
    )
    .await
    .expect_err("a different upstream identity must not reauthorize this account");

    assert_eq!(error, CodexOAuthAdminError::IdentityMismatch);
    assert_identity_and_credential_unchanged(&store, &account_id, &before).await;
}

/// 新 id_token 完全没有身份 claims（无 auth section）时必须拒绝；否则越权凭据
/// 会以“无 claim 即跳过比较”的方式静默通过。
#[tokio::test]
async fn reauthorization_without_new_identity_claims_is_rejected() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_reauth_missing_claims".to_owned(),
            name: "reauthorization missing claims".to_owned(),
            secret: secret("reauth-missing-claims-access"),
            verified_account: account_profile("chatgpt-reauth-missing"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account_id = ProviderAccountId::new("acct_reauth_missing_claims").expect("account id");
    let before = store
        .load_current_credential(&account_id)
        .await
        .expect("seeded credential");

    for payload in [
        // 完全没有 auth section。
        serde_json::json!({ "email": "identity@example.com" }),
        // 有 auth section 但没有任何身份字段。
        serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_plan_type": "pro" }
        }),
    ] {
        let error = complete_reauthorization(
            store.clone(),
            account_id.clone(),
            id_token(payload),
            "reauth-identity-missing-claims",
        )
        .await
        .expect_err("a new token without identity claims must not reauthorize");
        assert_eq!(error, CodexOAuthAdminError::IdentityMismatch);
        assert_identity_and_credential_unchanged(&store, &account_id, &before).await;
    }
}

/// 行侧 `upstream_user_id` 为 NULL 时没有可比对的基线，必须拒绝而不是放行。
#[tokio::test]
async fn reauthorization_without_a_stored_upstream_user_id_is_rejected() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_account_without_upstream_identity(
        &store,
        "acct_reauth_null_user",
        None,
        Some("chatgpt-null-user-workspace"),
    )
    .await;
    let account_id = ProviderAccountId::new("acct_reauth_null_user").expect("account id");
    let before = store
        .load_current_credential(&account_id)
        .await
        .expect("seeded credential");

    let error = complete_reauthorization(
        store.clone(),
        account_id.clone(),
        id_token(serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_user_id": "chatgpt-null-user",
                "chatgpt_account_id": "chatgpt-null-user-workspace"
            }
        })),
        "reauth-identity-null-user",
    )
    .await
    .expect_err("a NULL baseline user id must not reauthorize");

    assert_eq!(error, CodexOAuthAdminError::IdentityMismatch);
    assert_identity_and_credential_unchanged(&store, &account_id, &before).await;
}

/// 用户相同、工作空间不同同样属于身份错配，必须拒绝。
#[tokio::test]
async fn reauthorization_with_a_different_upstream_account_id_is_rejected() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_reauth_account_id".to_owned(),
            name: "reauthorization account id".to_owned(),
            secret: secret("reauth-account-id-access"),
            verified_account: account_profile("chatgpt-reauth-account"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account_id = ProviderAccountId::new("acct_reauth_account_id").expect("account id");
    let before = store
        .load_current_credential(&account_id)
        .await
        .expect("seeded credential");

    let error = complete_reauthorization(
        store.clone(),
        account_id.clone(),
        id_token(serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_user_id": "user-chatgpt-reauth-account",
                "chatgpt_account_id": "another-workspace"
            }
        })),
        "reauth-identity-account-id",
    )
    .await
    .expect_err("a different upstream account id must not reauthorize");

    assert_eq!(error, CodexOAuthAdminError::IdentityMismatch);
    assert_identity_and_credential_unchanged(&store, &account_id, &before).await;
}

/// 同一身份的重新授权必须继续成功并轮换凭据——守卫不能把人工重授权整体打死。
#[tokio::test]
async fn reauthorization_with_the_same_identity_rotates_the_credential() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_reauth_same_identity".to_owned(),
            name: "reauthorization same identity".to_owned(),
            secret: secret("reauth-same-identity-access"),
            verified_account: account_profile("chatgpt-reauth-same"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account_id = ProviderAccountId::new("acct_reauth_same_identity").expect("account id");
    let before = store
        .load_current_credential(&account_id)
        .await
        .expect("seeded credential");

    let completed = complete_reauthorization(
        store.clone(),
        account_id.clone(),
        id_token(serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro",
                "chatgpt_user_id": "user-chatgpt-reauth-same",
                "chatgpt_account_id": "chatgpt-reauth-same"
            }
        })),
        "reauth-identity-same",
    )
    .await
    .expect("the same upstream identity must still reauthorize");

    let CompletedCodexOAuthCredential::Reauthorize(prepared) = completed else {
        panic!("expected a prepared credential rotation");
    };
    let (_, prepared_credential, replacement_identity, _guard) = prepared.into_parts();
    let parts = prepared_credential.into_parts();
    assert!(replacement_identity.is_none(), "重新授权不得替换账号身份列");
    let rotated = parts
        .credential
        .expose_to_provider()
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .expect("prepared access token");
    assert_ne!(
        rotated, "reauth-same-identity-access",
        "a matching reauthorization must still rotate the stored credential"
    );
    assert_eq!(parts.expected_revision, before.account.revision());
}

/// 行侧 `upstream_account_id` 为 NULL、新侧有值时没有可比对的工作空间基线，允许通过。
#[tokio::test]
async fn reauthorization_may_fill_a_missing_upstream_account_id() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_account_without_upstream_identity(
        &store,
        "acct_reauth_null_account",
        Some("chatgpt-null-account-user"),
        None,
    )
    .await;
    let account_id = ProviderAccountId::new("acct_reauth_null_account").expect("account id");

    let completed = complete_reauthorization(
        store.clone(),
        account_id.clone(),
        id_token(serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_user_id": "chatgpt-null-account-user",
                "chatgpt_account_id": "workspace-now-known"
            }
        })),
        "reauth-identity-null-account",
    )
    .await
    .expect("a missing stored workspace id must not block reauthorization");

    assert!(matches!(
        completed,
        CompletedCodexOAuthCredential::Reauthorize(_)
    ));
    let after = store
        .load_current_credential(&account_id)
        .await
        .expect("credential after reauthorization");
    assert_eq!(
        after.account.upstream_user_id(),
        Some("chatgpt-null-account-user")
    );
    assert_eq!(
        after.account.upstream_account_id(),
        None,
        "身份列在成功重新授权时保持原样"
    );
}

/// 断言拒绝路径没有写坏账号：身份列、凭据与 revision 全部保持原值。
async fn assert_identity_and_credential_unchanged(
    store: &Arc<MemoryAccountStore>,
    account_id: &ProviderAccountId,
    before: &gateway_core::account::LoadedCredential,
) {
    let after = store
        .load_current_credential(account_id)
        .await
        .expect("credential after rejection");
    assert_eq!(
        after.account.upstream_user_id(),
        before.account.upstream_user_id()
    );
    assert_eq!(
        after.account.upstream_account_id(),
        before.account.upstream_account_id()
    );
    assert_eq!(after.credential, before.credential);
    assert_eq!(after.account.revision(), before.account.revision());
}

/// 拒绝路径只写固定原因码与 account_id，不落任何 claim、邮箱或令牌。
#[tokio::test]
async fn reauthorization_identity_mismatch_logs_only_the_reason_code_and_account_id() {
    use std::io::{self, Write};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    impl Write for CapturedLogs {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| io::Error::other("captured logs lock poisoned"))?
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "mismatched-access",
            "refresh_token": "mismatched-refresh",
            "id_token": id_token(serde_json::json!({
                "email": "other-account@example.com",
                "https://api.openai.com/auth": {
                    "chatgpt_user_id": "chatgpt-other-user",
                    "chatgpt_account_id": "other-workspace-id"
                }
            }))
        })))
        .expect(1)
        .mount(&server)
        .await;

    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_reauth_logging".to_owned(),
            name: "reauthorization logging".to_owned(),
            secret: secret("reauth-logging-access"),
            verified_account: account_profile("chatgpt-reauth-logging"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account_id = ProviderAccountId::new("acct_reauth_logging").expect("account id");

    let mut config = provider_openai::OpenAiConfig::default();
    config.auth.oauth_token_endpoint = format!("{}/oauth/token", server.uri());
    let pending_store = Arc::new(PendingStore::default());
    let service = CodexOAuthAdminService::new(
        pending_store,
        Arc::new(
            provider_openai::credential::token_client::openai_token_client(
                config.token_client_config(),
                profile(),
            )
            .expect("token client"),
        ),
        store,
        CodexCredentialAdmin,
        profile(),
    );
    let started = service
        .start_authorization(StartCodexOAuthAuthorization {
            mutation: reauthorization_mutation(account_id, "reauth-identity-logging"),
        })
        .await
        .expect("start OAuth reauthorization");
    let state = Url::parse(&started.authorization_url)
        .expect("authorization URL")
        .query_pairs()
        .find_map(|(key, value)| (key == "authorize_url").then(|| value.into_owned()))
        .and_then(|inner| Url::parse(&inner).ok())
        .and_then(|inner| {
            inner
                .query_pairs()
                .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        })
        .expect("state parameter");

    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_writer(captured.clone())
        .finish();
    let error = {
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        service
            .complete_authorization(CompleteCodexOAuthAuthorization {
                owner_ref: "test-owner".to_owned(),
                flow_id: started.flow_id,
                callback_url: SecretString::from(format!(
                    "http://localhost:1455/auth/callback?code=code-from-browser&state={state}"
                )),
            })
            .await
            .expect_err("a different upstream identity must be rejected")
    };
    assert_eq!(error, CodexOAuthAdminError::IdentityMismatch);

    let logs = String::from_utf8(captured.0.lock().expect("captured logs lock").clone())
        .expect("captured logs are UTF-8");
    let guard_event = logs
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("log line is JSON"))
        .find(|event| {
            event
                .get("fields")
                .and_then(|fields| fields.get("reason"))
                .and_then(serde_json::Value::as_str)
                == Some("reauthorization_identity_mismatch")
        })
        .expect("the guard must emit its structured rejection event");
    assert_eq!(
        guard_event
            .get("fields")
            .and_then(|fields| fields.get("account_id"))
            .and_then(serde_json::Value::as_str),
        Some("acct_reauth_logging")
    );
    let guard_event = guard_event.to_string();
    for forbidden in [
        "chatgpt-other-user",
        "other-workspace-id",
        "other-account@example.com",
        "mismatched-access",
        "mismatched-refresh",
        "access_token",
        "refresh_token",
        "id_token",
        "chatgpt_user_id",
        "chatgpt_account_id",
    ] {
        assert!(
            !guard_event.contains(forbidden),
            "the guard log leaked {forbidden}: {guard_event}"
        );
    }
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
