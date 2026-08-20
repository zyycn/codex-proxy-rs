use gateway_store::StoreConfig;

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
