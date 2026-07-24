use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{TimeZone, Utc};
use ed25519_dalek::pkcs8::EncodePrivateKey as _;
use ed25519_dalek::{Signature, SigningKey, Verifier as _};
use gateway_core::engine::credential::{
    AccountAvailability, CredentialRevision, NewProviderAccount, ProviderAccount,
    ProviderAccountId, ProviderAccountStore,
};
use gateway_core::routing::ProviderKind;
use provider_openai::credential::{
    CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY, CodexAgentIdentityAuthMode,
    CodexAgentIdentityCredentialData, CodexAgentIdentityError, CodexAgentIdentitySecret,
    CodexAgentIdentityTaskRegistrar, CodexAgentIdentityTaskService, CodexCredentialCodec,
    CodexRuntimeAuthentication, OfficialCodexAgentIdentityTaskRegistrar,
    is_agent_identity_task_invalid_response,
};
use provider_openai::transport::CodexWebSocketPool;
use reqwest::StatusCode;
use secrecy::ExposeSecret as _;
use serde_json::Value;

use super::admin::{import_service, unused_import_refresher};
use crate::support::MemoryAccountStore;

fn signing_fixture(seed: u8) -> (SigningKey, String) {
    let signing_key = SigningKey::from_bytes(&[seed; 32]);
    let der = signing_key.to_pkcs8_der().expect("encode test PKCS#8 key");
    (signing_key, STANDARD.encode(der.as_bytes()))
}

fn agent_account(
    account_id: &str,
    runtime_id: &str,
    private_key: &str,
    task_id: Option<&str>,
) -> NewProviderAccount {
    let account_id = ProviderAccountId::new(account_id.to_owned()).expect("account id");
    let provider = ProviderKind::new("openai").expect("provider");
    let credential =
        CodexCredentialCodec::encode_agent_identity(CodexAgentIdentityCredentialData {
            schema_version: 1,
            auth_mode: CodexAgentIdentityAuthMode::AgentIdentity,
            installation_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            agent_runtime_id: runtime_id.to_owned(),
            agent_private_key: private_key.to_owned(),
            task_id: task_id.map(str::to_owned),
            cookies: Vec::new(),
        })
        .expect("encode Agent Identity credential");
    let account = ProviderAccount::new(
        account_id,
        provider,
        "Agent Identity test".to_owned(),
        "agent-user".to_owned(),
        CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY.to_owned(),
        CredentialRevision::new(1).expect("revision"),
        None,
    )
    .with_profile(
        Some("agent@example.com".to_owned()),
        Some("agent-account".to_owned()),
        Some("pro".to_owned()),
    )
    .with_runtime_state(true, AccountAvailability::Ready, None)
    .with_refresh_schedule(false, None);
    NewProviderAccount {
        account,
        credential,
    }
}

#[test]
fn agent_identity_secret_builds_a_verifiable_assertion_without_leaking_material() {
    let (signing_key, private_key) = signing_fixture(7);
    let secret = CodexAgentIdentitySecret::from_pkcs8(
        "runtime-test".to_owned(),
        &private_key,
        Some("task-test".to_owned()),
    )
    .expect("parse Agent Identity secret");
    let now = Utc
        .with_ymd_and_hms(2026, 7, 23, 1, 2, 3)
        .single()
        .expect("timestamp");
    let header = secret
        .authorization_header(now)
        .expect("build AgentAssertion");
    let encoded = header
        .expose_secret()
        .strip_prefix("AgentAssertion ")
        .expect("AgentAssertion scheme");
    let envelope: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(encoded)
            .expect("decode assertion envelope"),
    )
    .expect("parse assertion envelope");
    assert_eq!(envelope["agent_runtime_id"], "runtime-test");
    assert_eq!(envelope["task_id"], "task-test");
    assert_eq!(envelope["timestamp"], "2026-07-23T01:02:03Z");
    let signature = Signature::from_slice(
        &STANDARD
            .decode(envelope["signature"].as_str().expect("signature"))
            .expect("decode signature"),
    )
    .expect("signature shape");
    signing_key
        .verifying_key()
        .verify(b"runtime-test:task-test:2026-07-23T01:02:03Z", &signature)
        .expect("verify AgentAssertion");
    let debug = format!("{secret:?}");
    assert!(!debug.contains(&private_key));
    assert!(!debug.contains("runtime-test"));
    assert!(!debug.contains("task-test"));
}

#[test]
fn agent_identity_task_error_classifier_requires_unauthorized_task_error() {
    assert!(is_agent_identity_task_invalid_response(
        StatusCode::UNAUTHORIZED,
        r#"{"error":{"code":"task_not_found"}}"#,
    ));
    assert!(is_agent_identity_task_invalid_response(
        StatusCode::UNAUTHORIZED,
        "task expired",
    ));
    assert!(!is_agent_identity_task_invalid_response(
        StatusCode::FORBIDDEN,
        r#"{"error":{"code":"task_not_found"}}"#,
    ));
    assert!(!is_agent_identity_task_invalid_response(
        StatusCode::UNAUTHORIZED,
        "invalid access token",
    ));
}

struct SequenceTaskRegistrar {
    task_ids: Mutex<VecDeque<String>>,
    calls: AtomicUsize,
}

#[async_trait]
impl CodexAgentIdentityTaskRegistrar for SequenceTaskRegistrar {
    async fn register(
        &self,
        _credential: &CodexAgentIdentitySecret,
    ) -> Result<String, CodexAgentIdentityError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.task_ids
            .lock()
            .expect("task registrar lock")
            .pop_front()
            .ok_or(CodexAgentIdentityError::TaskRegistrationUnavailable)
    }
}

fn task_service(
    store: &Arc<MemoryAccountStore>,
    registrar: Arc<SequenceTaskRegistrar>,
) -> CodexAgentIdentityTaskService {
    CodexAgentIdentityTaskService::new(
        store.repository(),
        registrar,
        Arc::new(CodexWebSocketPool::default()),
    )
}

#[tokio::test]
async fn task_registration_and_recovery_are_revision_fenced() {
    let (signing_key, private_key) = signing_fixture(8);
    let runtime_id = format!(
        "runtime-{}",
        hex::encode(signing_key.verifying_key().as_bytes())
    );
    let store = Arc::new(MemoryAccountStore::default());
    store
        .create_account(agent_account(
            "acct_agent_task",
            &runtime_id,
            &private_key,
            None,
        ))
        .await
        .expect("seed Agent Identity account");
    let registrar = Arc::new(SequenceTaskRegistrar {
        task_ids: Mutex::new(VecDeque::from([
            "task-first".to_owned(),
            "task-recovered".to_owned(),
        ])),
        calls: AtomicUsize::new(0),
    });
    let service = task_service(&store, Arc::clone(&registrar));
    let current = store.account("acct_agent_task").expect("current account");
    let prepared = service.prepare(&current).await.expect("register task");
    assert_eq!(prepared.account.revision().get(), 2);
    let CodexRuntimeAuthentication::AgentIdentity(secret) = prepared.credential.authentication
    else {
        panic!("expected Agent Identity runtime credential");
    };
    assert_eq!(secret.task_id(), Some("task-first"));
    assert_eq!(registrar.calls.load(Ordering::SeqCst), 1);

    let current = store.account("acct_agent_task").expect("updated account");
    let prepared = service
        .prepare(&current)
        .await
        .expect("reuse registered task");
    assert_eq!(prepared.account.revision().get(), 2);
    assert_eq!(registrar.calls.load(Ordering::SeqCst), 1);

    let recovered = service
        .recover(prepared.account.id(), "task-first")
        .await
        .expect("rejected task must register a replacement");
    assert_eq!(recovered.account.revision().get(), 3);
    assert_eq!(registrar.calls.load(Ordering::SeqCst), 2);
    let CodexRuntimeAuthentication::AgentIdentity(secret) = recovered.credential.authentication
    else {
        panic!("expected Agent Identity runtime credential");
    };
    assert_eq!(secret.task_id(), Some("task-recovered"));

    let fenced = service
        .recover(recovered.account.id(), "task-first")
        .await
        .expect("stale task recovery must keep the newer task");
    assert_eq!(fenced.account.revision().get(), 3);
    assert_eq!(registrar.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn agent_identity_import_accepts_the_minimal_document_shape() {
    let prepared = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "auth_mode": "agentIdentity",
            "agent_identity": {
                "agent_runtime_id": "runtime-minimal",
                "agent_private_key": signing_fixture(9).1,
                "account_id": "chatgpt-minimal",
                "chatgpt_user_id": "user-minimal",
                "email": "minimal@example.com",
                "plan_type": "pro"
            }
        }))
        .await
        .expect("minimal Agent Identity import");
    assert_eq!(prepared.accounts().len(), 1);
    let account = &prepared.accounts()[0].account;
    assert_eq!(
        account.authentication_kind(),
        CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY
    );
    assert_eq!(account.access_token_expires_at(), None);
    assert!(!format!("{prepared:?}").contains("agent_private_key"));
}

#[tokio::test]
#[ignore = "requires CODEX_AGENT_IDENTITY_FIXTURE and CODEX_SUB2API_AGENT_FIXTURE; the first also registers a live task"]
async fn real_agent_identity_documents_import_and_register_contract() {
    let paths = [
        std::env::var("CODEX_AGENT_IDENTITY_FIXTURE").expect("minimal Agent Identity fixture"),
        std::env::var("CODEX_SUB2API_AGENT_FIXTURE").expect("Sub2API Agent Identity fixture"),
    ];
    let store = Arc::new(MemoryAccountStore::default());
    let mut imported_accounts = Vec::new();
    for path in paths {
        let payload: Value =
            serde_json::from_slice(&std::fs::read(path).expect("read Agent Identity fixture"))
                .expect("parse Agent Identity fixture");
        let prepared = import_service(unused_import_refresher())
            .prepare_import_document(payload)
            .await
            .expect("prepare real Agent Identity import");
        assert_eq!(prepared.accounts().len(), 1);
        let account = prepared.accounts().first().expect("imported account");
        assert_eq!(
            account.account.authentication_kind(),
            CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY
        );
        assert!(!format!("{prepared:?}").contains("agent_private_key"));
        store
            .create_account(NewProviderAccount {
                account: account.account.clone(),
                credential: account.credential.clone(),
            })
            .await
            .expect("store imported Agent Identity account");
        imported_accounts.push(account.account.clone());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("task registration client");
    let registrar =
        Arc::new(OfficialCodexAgentIdentityTaskRegistrar::new(client).expect("official registrar"));
    let live_service = CodexAgentIdentityTaskService::new(
        store.repository(),
        registrar,
        Arc::new(CodexWebSocketPool::default()),
    );
    for account in imported_accounts {
        let _ = live_service
            .prepare(&account)
            .await
            .expect("register or reuse live Agent Identity task");
    }
}
