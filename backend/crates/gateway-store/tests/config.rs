use gateway_store::{StoreConfig, StoreError, StorePoolConfig};

const PASSWORD: &str = "111111111111111111111111111111111111111111111111";

#[test]
fn store_config_should_derive_backup_staging_from_runtime_data_dir() {
    let root = tempfile::tempdir().expect("runtime data root");
    let mut config = valid_config();

    config
        .resolve_and_validate(root.path())
        .expect("valid Store configuration");

    assert_eq!(
        config.backup_staging_dir(),
        root.path().join("backup-staging")
    );
}

#[test]
fn store_pool_config_keeps_the_public_surface_to_connection_pool_limits() {
    let pool: StorePoolConfig =
        serde_json::from_value(serde_json::json!({})).expect("default pool configuration");

    assert_eq!(pool, StorePoolConfig::default());
    assert_eq!(pool.max_connections, 20);
    assert_eq!(pool.acquire_timeout_seconds, 5);
    assert_eq!(pool.observability_max_connections(), 16);
    assert_eq!(
        StorePoolConfig {
            max_connections: 50,
            ..pool
        }
        .observability_max_connections(),
        40
    );
    assert!(
        serde_json::from_value::<StorePoolConfig>(serde_json::json!({
            "statement_timeout_seconds": 60,
        }))
        .is_err()
    );
}

#[test]
fn store_config_rejects_a_pool_that_cannot_reserve_both_traffic_classes() {
    let root = tempfile::tempdir().expect("runtime data root");
    let mut config: StoreConfig = serde_json::from_value(serde_json::json!({
        "database": {
            "url": "postgres://codex_proxy@127.0.0.1:5432/codex_proxy",
            "password": PASSWORD,
        },
        "redis": {
            "url": "redis://127.0.0.1:6379/",
            "password": PASSWORD,
        },
        "pool": {
            "max_connections": 1,
        },
    }))
    .expect("syntactically valid Store configuration");

    let error = config
        .resolve_and_validate(root.path())
        .expect_err("one connection cannot serve both traffic classes");

    assert!(matches!(
        error,
        StoreError::InvalidData { message, .. }
            if message.contains("max_connections must be at least 2")
    ));
}

fn valid_config() -> StoreConfig {
    serde_json::from_value(serde_json::json!({
        "database": {
            "url": "postgres://codex_proxy@127.0.0.1:5432/codex_proxy",
            "password": PASSWORD,
        },
        "redis": {
            "url": "redis://127.0.0.1:6379/",
            "password": PASSWORD,
        },
    }))
    .expect("test Store configuration")
}
