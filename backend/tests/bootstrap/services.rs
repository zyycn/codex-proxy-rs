use std::process::Command;

use crate::support::{
    config::test_config,
    storage::{background_task_stores, create_test_redis, init_test_db, test_database_url},
};
use codex_proxy_rs::{
    bootstrap::services::Services, upstream::openai::transport::tls::CODEX_CA_CERT_ENV,
};

#[tokio::test]
async fn services_try_new_should_use_configured_tls_transport_builder() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_SERVICES_TLS_CASE";

    if std::env::var(CASE_ENV).as_deref() == Ok("invalid_ca") {
        let (pool, _temp_dir) = init_test_db("services-tls").await;
        let config = test_config(test_database_url());
        let redis = create_test_redis("services-tls").await;
        let stores = background_task_stores(pool, redis);

        let Err(error) = Services::try_new(
            &config,
            stores,
            crate::support::wire_profile::test_wire_profile(),
        ) else {
            panic!("invalid custom CA should fail service transport construction");
        };
        assert!(
            error
                .to_string()
                .contains("Failed to read CA certificate file")
        );
        return;
    }

    let current_exe = std::env::current_exe().expect("current test binary path");
    let output = Command::new(current_exe)
        .arg("--exact")
        .arg("bootstrap::services::services_try_new_should_use_configured_tls_transport_builder")
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
    let (pool, _temp_dir) = init_test_db("services-ws-pool-enabled").await;
    let config = test_config(test_database_url());
    let redis = create_test_redis("services-ws-pool-enabled").await;
    let services = Services::try_new(
        &config,
        background_task_stores(pool, redis),
        crate::support::wire_profile::test_wire_profile(),
    )
    .expect("services should build");

    assert!(services.websocket_pool.is_some());
}

#[tokio::test]
async fn services_try_new_should_not_expose_websocket_pool_when_disabled() {
    let (pool, _temp_dir) = init_test_db("services-ws-pool-disabled").await;
    let mut config = test_config(test_database_url());
    let redis = create_test_redis("services-ws-pool-disabled").await;
    config.ws_pool.enabled = false;
    let services = Services::try_new(
        &config,
        background_task_stores(pool, redis),
        crate::support::wire_profile::test_wire_profile(),
    )
    .expect("services should build");

    assert!(services.websocket_pool.is_none());
}
