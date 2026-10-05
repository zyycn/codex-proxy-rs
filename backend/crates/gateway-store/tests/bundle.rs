//! 验证只读检查 Bundle 不执行迁移且拒绝业务写入

use gateway_core::{
    account::{
        CredentialRevision, NewProviderAccount, PlaintextCredential, ProviderAccount,
        ProviderAccountId,
    },
    routing::ProviderKind,
};
use gateway_store::StoreConfig;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
async fn inspection_bundle_does_not_migrate_and_rejects_business_writes() {
    let (Some(database), Some(redis)) = (
        crate::support::test_env("CPR_TEST_DATABASE_URL"),
        crate::support::test_env("CPR_TEST_REDIS_URL"),
    ) else {
        return;
    };
    let schema = format!("cpr_inspection_{}", uuid::Uuid::new_v4().simple());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create schema {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let mut database = url::Url::parse(&database).unwrap();
    database
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let config_connection = |mut url: url::Url| {
        let password = url.password().unwrap().to_owned();
        url.set_password(None).unwrap();
        json!({"url":url.as_str(),"password":password})
    };
    let mut config: StoreConfig = serde_json::from_value(json!({
        "database":config_connection(database), "redis":config_connection(url::Url::parse(&redis).unwrap()),
    })).unwrap();
    let directory = tempfile::tempdir().unwrap();
    config.resolve_and_validate(directory.path()).unwrap();
    let inspection = gateway_store::initialize_read_only(config.clone())
        .await
        .unwrap();
    assert!(
        inspection
            .admin_ports()
            .plugins()
            .load_instances()
            .await
            .is_err()
    );
    let tables: i64 =
        sqlx::query_scalar("select count(*) from information_schema.tables where table_schema=$1")
            .bind(&schema)
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(tables, 0, "只读帮助不能在空库创建迁移表或业务表");
    drop(inspection);
    let writable = gateway_store::initialize(config.clone()).await.unwrap();
    let inspection = gateway_store::initialize_read_only(config).await.unwrap();
    let create = |id: &str| NewProviderAccount {
        account: ProviderAccount::new(
            ProviderAccountId::new(id.to_owned()).unwrap(),
            ProviderKind::new("example").unwrap(),
            "inspection test".into(),
            None,
            "api_key".into(),
            CredentialRevision::new(1).unwrap(),
            None,
        ),
        credential: PlaintextCredential::new(
            json!({"key":"test-only"}).as_object().unwrap().clone(),
        ),
        model_access: None,
    };
    writable
        .provider_ports()
        .accounts()
        .create_account(create("acct_writable"))
        .await
        .unwrap();
    assert!(
        inspection
            .provider_ports()
            .accounts()
            .create_account(create("acct_readonly"))
            .await
            .is_err()
    );
    let accounts = inspection
        .provider_ports()
        .accounts()
        .list_accounts()
        .await
        .unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id().as_str(), "acct_writable");
    drop(inspection);
    drop(writable);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("drop schema {schema} cascade")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
