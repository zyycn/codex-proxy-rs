use super::*;
use gateway_core::account::AccountConcurrencyLimit;
use gateway_store::postgres::RecoverProviderAccount;

#[tokio::test]
async fn cross_kind_observations_preserve_time_bounds_and_stale_observation_fences() {
    let Some(database) = TestDatabase::create("account_observation_time_bounds").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let account_id = ProviderAccountId::new("acct_observation_time_bounds").expect("account id");
    let mut seed = account(account_id.as_str(), "user-observation-time-bounds");
    // 合成应用时钟领先数据库的已有事实，避免修改宿主时钟或依赖测试执行速度。
    let anchor = Utc::now() + TimeDelta::hours(1);
    seed.credential_observed_at = anchor;
    repository
        .insert_provider_account(seed)
        .await
        .expect("seed account");
    let revision = CredentialRevision::new(1).expect("revision");
    let quota_at = SystemTime::from(anchor - TimeDelta::milliseconds(400));
    let observation = QuotaObservation {
        plan_type: None,
        account_id: account_id.clone(),
        expected_revision: revision,
        quota: OpaqueProviderData::new(json!({"marker": "current"}).as_object().unwrap().clone()),
        observed_at: quota_at,
        state: QuotaState::allowed(quota_at),
    };
    assert_eq!(
        repository
            .compare_and_swap_quota(observation.clone())
            .await
            .expect("quota before credential observation"),
        QuotaWriteOutcome::Updated
    );
    assert_eq!(
        repository
            .touch_quota_observation(QuotaObservationTouch {
                account_id: account_id.clone(),
                expected_revision: revision,
                observed_at: quota_at + Duration::from_millis(100),
            })
            .await
            .expect("quota touch before credential observation"),
        QuotaWriteOutcome::Updated
    );
    assert_eq!(
        repository
            .apply_quota_access(QuotaAccessChange {
                account_id: account_id.clone(),
                expected_revision: revision,
                state: QuotaState::allowed(quota_at + Duration::from_millis(200)),
            })
            .await
            .expect("access before credential observation"),
        QuotaWriteOutcome::Updated
    );

    let access_at = SystemTime::from(anchor + TimeDelta::seconds(1));
    let mut newer = observation.clone();
    newer.observed_at = SystemTime::from(anchor + TimeDelta::milliseconds(500));
    newer.state = QuotaState::exhausted(QuotaEvidence::UsageLimitReached, access_at, None);
    assert_eq!(
        repository
            .compare_and_swap_quota(newer)
            .await
            .expect("access time may exceed quota document time"),
        QuotaWriteOutcome::Updated
    );
    repository
        .apply_state_change(AccountStateChange {
            account_id: account_id.clone(),
            expected_revision: revision,
            credential_state: CredentialState::Ready,
            observed_at: SystemTime::from(anchor + TimeDelta::milliseconds(600)),
            error_reason: None,
            message: None,
        })
        .await
        .expect("credential update preserves later quota access time");
    assert_eq!(
        repository
            .compare_and_swap_quota(observation)
            .await
            .expect("stale quota remains a conflict"),
        QuotaWriteOutcome::Conflict
    );
    assert_eq!(
        repository
            .apply_quota_access(QuotaAccessChange {
                account_id: account_id.clone(),
                expected_revision: revision,
                state: QuotaState::allowed(quota_at),
            })
            .await
            .expect("stale access remains a conflict"),
        QuotaWriteOutcome::Conflict
    );
    assert!(
        repository
            .apply_state_change(AccountStateChange {
                account_id: account_id.clone(),
                expected_revision: revision,
                credential_state: CredentialState::Invalid,
                observed_at: SystemTime::from(anchor),
                error_reason: Some(AccountErrorReason::CredentialInvalid),
                message: None,
            })
            .await
            .is_err()
    );
    let stored = repository
        .load_provider_account(account_id.as_str())
        .await
        .expect("load bounded account")
        .expect("account");
    assert_eq!(
        stored.summary.updated_at.timestamp_micros(),
        chrono::DateTime::<Utc>::from(access_at).timestamp_micros()
    );
    assert_eq!(stored.summary.credential_state, CredentialState::Ready);
    let quota = repository
        .get_quotas(std::slice::from_ref(&account_id))
        .await
        .expect("load quota")
        .pop()
        .expect("quota");
    assert_eq!(quota.state.access(), QuotaAccessState::Exhausted);
    database.close().await;
}

#[tokio::test]
async fn credential_and_admin_writes_preserve_existing_update_time_after_clock_rollback() {
    let Some(database) = TestDatabase::create("account_update_clock_rollback").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let account_id = ProviderAccountId::new("acct_update_clock_rollback").expect("account id");
    let mut seed = account(account_id.as_str(), "user-update-clock-rollback");
    let anchor = Utc::now() + TimeDelta::hours(1);
    seed.credential_observed_at = anchor;
    repository
        .insert_provider_account(seed)
        .await
        .expect("seed account");
    // 账号在回拨前创建，管理恢复和重新导入也必须保留这一时间下界。
    sqlx::query("update provider_accounts set created_at = updated_at where id = $1")
        .bind(account_id.as_str())
        .execute(&database.pool)
        .await
        .expect("seed creation before rollback");
    assert!(
        repository
            .update_provider_account(profile(account_id.as_str(), "after rollback"))
            .await
            .expect("update profile")
    );
    assert!(
        repository
            .set_provider_account_enabled(account_id.as_str(), false)
            .await
            .expect("disable account")
    );
    assert!(
        repository
            .set_provider_account_enabled(account_id.as_str(), true)
            .await
            .expect("enable account")
    );
    assert_eq!(
        repository
            .compare_and_swap_credentials(credential_update(
                account_id.as_str(),
                1,
                "synthetic-rotated"
            ))
            .await
            .expect("rotate credentials")
            .get(),
        2
    );
    let refreshed = CredentialCasUpdate::new(
        account_id.clone(),
        CredentialRevision::new(2).expect("revision"),
        ProviderAccountUpdate {
            account_id: account_id.clone(),
            name: "refreshed".to_owned(),
            email: None,
            plan_type: None,
        },
        plaintext_credential("synthetic-refreshed"),
        false,
        None,
        None,
    )
    .expect("refresh update")
    .with_account_state(CredentialState::Ready, SystemTime::now(), None, None);
    assert_eq!(
        repository
            .compare_and_swap_credential(refreshed.clone())
            .await
            .expect("core credential refresh"),
        CredentialCasOutcome::Updated(CredentialRevision::new(3).expect("new revision"))
    );
    assert_eq!(
        repository
            .compare_and_swap_credential(refreshed)
            .await
            .expect("stale refresh conflicts"),
        CredentialCasOutcome::Conflict
    );
    assert!(
        admin_account_store(&database.pool)
            .lower_concurrency_limit(
                &account_id,
                AccountConcurrencyLimit::new(1).expect("limit"),
                &MutationContext {
                    actor: MutationActor::System,
                    request_id: "rollback-concurrency".to_owned()
                },
            )
            .await
            .expect("lower concurrency after rollback")
            .is_some()
    );
    repository
        .recover_provider_account_admin(RecoverProviderAccount {
            account_id: account_id.as_str().to_owned(),
            audit: audit("audit_rollback_recover", "recover", account_id.as_str()),
        })
        .await
        .expect("recover account after rollback");
    let imported = repository
        .import_provider_accounts(ImportProviderAccounts {
            settings: None,
            outbound_proxy: None,
            scope: ProviderAccountAdminScope {
                provider_kind: "openai".to_owned(),
            },
            accounts: vec![account(
                "acct_reimport_clock_rollback",
                "user-update-clock-rollback",
            )],
            audit: audit("audit_rollback_import", "import", account_id.as_str()),
        })
        .await
        .expect("reimport account after rollback");
    assert_eq!(imported.account_ids, [account_id.as_str()]);
    let stored = repository
        .load_provider_account(account_id.as_str())
        .await
        .expect("load account")
        .expect("account");
    assert_eq!(
        stored.summary.updated_at.timestamp_micros(),
        anchor.timestamp_micros()
    );
    assert_eq!(stored.summary.credential_revision.get(), 4);
    database.close().await;
}
