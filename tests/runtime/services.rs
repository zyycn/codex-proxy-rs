use std::process::Command;

use codex_proxy_rs::{
    admin::{
        auth::service::SqliteAdminSessionStore, keys::service::SqliteClientKeyStore,
        monitoring::event_store::SqliteEventLogStore,
    },
    infra::{crypto::SecretBox, identity::ApiKeyHasher},
    runtime::services::{BackgroundTaskStores, Services},
    upstream::{
        accounts::{
            cookies::SqliteCookieStore, store::SqliteAccountStore, token_refresh::RefreshLeaseStore,
        },
        fingerprint::FingerprintRepository,
        transport::CODEX_CA_CERT_ENV,
    },
};

use crate::support::{config::test_config, sqlite::init_test_db};

#[tokio::test]
async fn services_try_new_should_use_configured_tls_transport_builder() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_SERVICES_TLS_CASE";

    if std::env::var(CASE_ENV).as_deref() == Ok("invalid_ca") {
        let (pool, temp_dir) = init_test_db("services-tls.sqlite").await;
        let config = test_config(format!(
            "sqlite://{}",
            temp_dir.path().join("services-tls.sqlite").display()
        ));
        let secret_box = SecretBox::load_or_create(temp_dir.path().join("master.key")).unwrap();
        let hasher = ApiKeyHasher::load_or_create(temp_dir.path().join("api-key-pepper.key"))
            .expect("api key hasher");
        let stores = BackgroundTaskStores {
            accounts: SqliteAccountStore::new(pool.clone(), secret_box.clone()),
            admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
            cookies: SqliteCookieStore::new(pool.clone(), secret_box),
            fingerprints: FingerprintRepository::new(pool.clone()),
            session_affinity:
                codex_proxy_rs::proxy::dispatch::session_affinity::SqliteSessionAffinityStore::new(
                    pool.clone(),
                ),
            refresh_leases: RefreshLeaseStore::new(pool.clone()),
            client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
            event_logs: SqliteEventLogStore::new(pool),
        };

        let error = match Services::try_new(
            &config,
            stores,
            crate::support::fingerprint::test_fingerprint(),
        ) {
            Ok(_) => panic!("invalid custom CA should fail service transport construction"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("Failed to read CA certificate file"));
        return;
    }

    let current_exe = std::env::current_exe().expect("current test binary path");
    let output = Command::new(current_exe)
        .arg("--exact")
        .arg("runtime::services::services_try_new_should_use_configured_tls_transport_builder")
        .arg("--nocapture")
        .env(CASE_ENV, "invalid_ca")
        .env(CODEX_CA_CERT_ENV, "/tmp/codex-proxy-rs-missing-ca.pem")
        .output()
        .expect("run isolated services TLS test case");

    assert!(
        output.status.success(),
        "isolated services TLS test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
