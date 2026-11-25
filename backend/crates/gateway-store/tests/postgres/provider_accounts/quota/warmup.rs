//! 额度预热声明的持久化与版本隔离测试

use super::*;

#[tokio::test]
async fn quota_warmup_claim_is_persistent_and_fenced_by_configuration_usage_and_account_state() {
    use gateway_core::account::QuotaWarmupStateUpdate;
    let Some(database) = TestDatabase::create("quota_warmup_claim").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    repository
        .insert_provider_account(account("acct_warmup", "fixture"))
        .await
        .unwrap();
    let id = ProviderAccountId::new("acct_warmup").unwrap();
    let current = repository.get_account(&id).await.unwrap().unwrap();
    let initial = repository
        .load_quota_warmup_state(&current)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial.version, 0);
    let claim = || QuotaWarmupStateUpdate {
        account: &current,
        expected_version: 0,
        expected_quota_observed_at: None,
        data: json!({"claimed": true}).as_object().unwrap().clone(),
    };
    let (one, two) = tokio::join!(
        repository.compare_and_swap_quota_warmup_state(claim()),
        repository.compare_and_swap_quota_warmup_state(claim())
    );
    assert_ne!(one.unwrap(), two.unwrap());
    let reloaded = PgProviderAccountRepository::new(database.pool.clone());
    let state = reloaded
        .load_quota_warmup_state(&current)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.version, 1);
    assert_eq!(state.data["claimed"], true);
    for mutation in [
        "update provider_accounts set quota_settings = '{\"codex:604800s\":{\"warmup\":{\"model\":\"test-model\"}}}' where id = 'acct_warmup'",
        "update provider_accounts set quota_observed_at = now(), updated_at = now(), provider_quota_json = '{}' where id = 'acct_warmup'",
        "update provider_accounts set enabled = false where id = 'acct_warmup'",
        "update provider_accounts set credential_state = 'invalid' where id = 'acct_warmup'",
        "update provider_accounts set credential_revision = credential_revision + 1 where id = 'acct_warmup'",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(mutation))
            .execute(&database.pool)
            .await
            .unwrap();
        assert!(
            !reloaded
                .compare_and_swap_quota_warmup_state(QuotaWarmupStateUpdate {
                    expected_version: 1,
                    ..claim()
                })
                .await
                .unwrap()
        );
        sqlx::query("update provider_accounts set quota_settings = '{}', quota_observed_at = null, provider_quota_json = null, enabled = true, credential_state = $1, credential_revision = $2 where id = 'acct_warmup'")
            .bind(current.credential_state().as_str()).bind(i64::try_from(current.revision().get()).unwrap())
            .execute(&database.pool).await.unwrap();
    }
    database.close().await;
}
