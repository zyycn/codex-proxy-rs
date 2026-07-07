use std::process::Command;

use codex_proxy_rs::{
    admin::{
        auth::service::SqliteAdminSessionStore, keys::service::SqliteClientKeyStore,
        monitoring::usage_record_store::SqliteUsageRecordStore,
    },
    runtime::services::{BackgroundTaskStores, Services},
    upstream::{
        accounts::{
            cookies::SqliteCookieStore, store::SqliteAccountStore, token_refresh::RefreshLeaseStore,
        },
        fingerprint::FingerprintRepository,
        transport::tls::CODEX_CA_CERT_ENV,
    },
};
use sqlx::SqlitePool;

use crate::support::{config::test_config, sqlite::init_test_db};

fn stores(pool: SqlitePool) -> BackgroundTaskStores {
    BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity:
            codex_proxy_rs::proxy::dispatch::session_affinity::SqliteSessionAffinityStore::new(
                pool.clone(),
            ),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool),
    }
}

#[tokio::test]
async fn services_try_new_should_use_configured_tls_transport_builder() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_SERVICES_TLS_CASE";

    if std::env::var(CASE_ENV).as_deref() == Ok("invalid_ca") {
        let (pool, temp_dir) = init_test_db("services-tls.sqlite").await;
        let config = test_config(format!(
            "sqlite://{}",
            temp_dir.path().join("services-tls.sqlite").display()
        ));
        let stores = stores(pool);

        let Err(error) = Services::try_new(
            &config,
            stores,
            crate::support::fingerprint::runtime_test_fingerprint(),
        ) else {
            panic!("invalid custom CA should fail service transport construction");
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

#[tokio::test]
async fn services_try_new_should_expose_websocket_pool_when_enabled() {
    let (pool, temp_dir) = init_test_db("services-ws-pool-enabled.sqlite").await;
    let config = test_config(format!(
        "sqlite://{}",
        temp_dir
            .path()
            .join("services-ws-pool-enabled.sqlite")
            .display()
    ));
    let services = Services::try_new(
        &config,
        stores(pool),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .expect("services should build");

    assert!(services.websocket_pool.is_some());
}

#[tokio::test]
async fn services_try_new_should_not_expose_websocket_pool_when_disabled() {
    let (pool, temp_dir) = init_test_db("services-ws-pool-disabled.sqlite").await;
    let mut config = test_config(format!(
        "sqlite://{}",
        temp_dir
            .path()
            .join("services-ws-pool-disabled.sqlite")
            .display()
    ));
    config.ws_pool.enabled = false;
    let services = Services::try_new(
        &config,
        stores(pool),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .expect("services should build");

    assert!(services.websocket_pool.is_none());
}
