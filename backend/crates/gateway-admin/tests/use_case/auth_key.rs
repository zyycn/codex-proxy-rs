use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use gateway_admin::{
    model::auth::{AdminAuditEvent, AuthSession, LoginCommand, LoginError, SessionSubject},
    ports::store::{AdminStoreResult, AuthStore},
};
use gateway_core::{
    engine::execution::{ClientAuthenticationError, ClientKeyVerifier},
    policy::ClientApiKeyId,
};

#[test]
fn client_login_command_debug_should_redact_api_key() {
    let command = LoginCommand::Key {
        api_key: "cpr_should_never_be_logged".to_owned(),
    };

    let rendered = format!("{command:?}");

    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("cpr_should_never_be_logged"));
}

#[tokio::test]
async fn login_should_create_key_bound_session_with_fixed_ttl() {
    let key = client_key("key-42");
    let store = Arc::new(MemoryKeyAuthStore::new(Some(key.clone())));
    let verifier = Arc::new(FixtureVerifier::accepting(key.clone()));
    let services = super::AdminHarness::new()
        .auth(store.clone())
        .client_key_verifier(verifier.clone())
        .client_session_ttl_minutes(30)
        .build()
        .await;

    let login = services
        .auth()
        .login(
            login_command("valid-client-key"),
            Ipv4Addr::LOCALHOST.into(),
            None,
        )
        .await
        .expect("client login");

    assert!(login.session_id.starts_with("session_"));
    assert!(!login.session_id.contains("valid-client-key"));
    assert_eq!(verifier.calls(), 1);
    assert_eq!(
        store.session(&login.session_id).map(|value| value.subject),
        Some(SessionSubject::Key {
            client_key_id: key.clone()
        })
    );
    let remaining = login.session.expires_at - Utc::now();
    assert!(remaining > Duration::minutes(29));
    assert!(remaining <= Duration::minutes(30));
}

#[tokio::test]
async fn invalid_and_removed_keys_should_share_the_same_login_error() {
    let rejected_store = Arc::new(MemoryKeyAuthStore::new(Some(client_key("key-42"))));
    let rejected = super::AdminHarness::new()
        .auth(rejected_store.clone())
        .client_key_verifier(Arc::new(FixtureVerifier::rejecting()))
        .build()
        .await
        .auth()
        .login(
            login_command("unknown-key"),
            Ipv4Addr::LOCALHOST.into(),
            None,
        )
        .await;

    let removed = super::AdminHarness::new()
        .auth(Arc::new(MemoryKeyAuthStore::new(None)))
        .client_key_verifier(Arc::new(FixtureVerifier::accepting(
            ClientApiKeyId::new("removed-key").expect("key id"),
        )))
        .build()
        .await
        .auth()
        .login(
            login_command("disabled-key"),
            Ipv4Addr::LOCALHOST.into(),
            None,
        )
        .await;

    assert_eq!(rejected, Err(LoginError::InvalidCredentials));
    assert_eq!(removed, Err(LoginError::InvalidCredentials));
}

#[tokio::test]
async fn rate_limit_should_reject_before_verifying_the_candidate_key() {
    let store = Arc::new(MemoryKeyAuthStore::new(Some(client_key("key-42"))));
    store.set_retry_after(Some(StdDuration::from_secs(37)));
    let verifier = Arc::new(FixtureVerifier::accepting(
        ClientApiKeyId::new("key-42").expect("key id"),
    ));
    let services = super::AdminHarness::new()
        .auth(store.clone())
        .client_key_verifier(verifier.clone())
        .build()
        .await;

    let result = services
        .auth()
        .login(
            login_command("valid-client-key"),
            Ipv4Addr::LOCALHOST.into(),
            None,
        )
        .await;

    assert_eq!(
        result,
        Err(LoginError::TooManyAttempts {
            retry_after_seconds: 37,
        })
    );
    assert_eq!(verifier.calls(), 0);
}

#[tokio::test]
async fn restored_session_should_be_revoked_when_its_key_is_no_longer_active() {
    let key = client_key("key-42");
    let store = Arc::new(MemoryKeyAuthStore::new(Some(key.clone())));
    let services = super::AdminHarness::new()
        .auth(store.clone())
        .client_key_verifier(Arc::new(FixtureVerifier::accepting(key.clone())))
        .build()
        .await;
    let login = services
        .auth()
        .login(
            login_command("valid-client-key"),
            Ipv4Addr::LOCALHOST.into(),
            None,
        )
        .await
        .expect("client login");
    store.replace_key(None);

    let session = services
        .auth()
        .session(Some(&login.session_id))
        .await
        .expect("session status");

    assert!(session.is_none());
    assert!(store.session(&login.session_id).is_none());
}

fn login_command(api_key: &str) -> LoginCommand {
    LoginCommand::Key {
        api_key: api_key.to_owned(),
    }
}

fn client_key(id: &str) -> ClientApiKeyId {
    ClientApiKeyId::new(id).expect("client key id")
}

struct FixtureVerifier {
    result: Result<ClientApiKeyId, ClientAuthenticationError>,
    calls: AtomicUsize,
}

impl FixtureVerifier {
    fn accepting(id: ClientApiKeyId) -> Self {
        Self {
            result: Ok(id),
            calls: AtomicUsize::new(0),
        }
    }

    fn rejecting() -> Self {
        Self {
            result: Err(ClientAuthenticationError::InvalidKey),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl ClientKeyVerifier for FixtureVerifier {
    fn verify_client_key(&self, _: &str) -> Result<ClientApiKeyId, ClientAuthenticationError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.clone()
    }
}

struct MemoryKeyAuthStore {
    key: Mutex<Option<ClientApiKeyId>>,
    sessions: Mutex<HashMap<String, AuthSession>>,
    retry_after: Mutex<Option<StdDuration>>,
}

impl MemoryKeyAuthStore {
    fn new(key: Option<ClientApiKeyId>) -> Self {
        Self {
            key: Mutex::new(key),
            sessions: Mutex::new(HashMap::new()),
            retry_after: Mutex::new(None),
        }
    }

    fn replace_key(&self, key: Option<ClientApiKeyId>) {
        *self.key.lock().expect("client key") = key;
    }

    fn set_retry_after(&self, retry_after: Option<StdDuration>) {
        *self.retry_after.lock().expect("retry after") = retry_after;
    }

    fn session(&self, session_id: &str) -> Option<AuthSession> {
        self.sessions
            .lock()
            .expect("client sessions")
            .get(session_id)
            .cloned()
    }
}

#[async_trait]
impl AuthStore for MemoryKeyAuthStore {
    async fn load_password_hash(&self, _: &str) -> AdminStoreResult<Option<String>> {
        Ok(None)
    }
    async fn change_password(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: gateway_admin::model::auth::AdminAuditEvent,
    ) -> AdminStoreResult<bool> {
        Ok(false)
    }

    async fn create_password_hash_if_absent(&self, _: &str, _: &str) -> AdminStoreResult<bool> {
        Ok(true)
    }
    async fn load_admin_api_key(
        &self,
    ) -> AdminStoreResult<Option<gateway_admin::model::settings::AdminApiKey>> {
        Ok(None)
    }
    async fn append_audit_event(&self, _: AdminAuditEvent) -> AdminStoreResult<()> {
        Ok(())
    }
    async fn client_key_enabled(&self, id: &ClientApiKeyId) -> AdminStoreResult<bool> {
        Ok(self
            .key
            .lock()
            .expect("client key")
            .as_ref()
            .is_some_and(|key| key == id))
    }
    async fn load_session(&self, session_id: &str) -> AdminStoreResult<Option<AuthSession>> {
        Ok(self.session(session_id))
    }

    async fn store_session(&self, session_id: &str, session: &AuthSession) -> AdminStoreResult<()> {
        self.sessions
            .lock()
            .expect("client sessions")
            .insert(session_id.to_owned(), session.clone());
        Ok(())
    }

    async fn delete_session(&self, session_id: &str) -> AdminStoreResult<Option<AuthSession>> {
        Ok(self
            .sessions
            .lock()
            .expect("client sessions")
            .remove(session_id))
    }

    async fn consume_login_attempt(
        &self,
        _: IpAddr,
        _: u32,
        _: u32,
        _: StdDuration,
    ) -> AdminStoreResult<Option<StdDuration>> {
        Ok(*self.retry_after.lock().expect("retry after"))
    }
}
