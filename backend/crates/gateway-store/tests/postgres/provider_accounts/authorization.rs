//! 验证账号授权回执的并发幂等、事务回滚与过期清理

use gateway_admin::model::provider_credentials::{
    AuthorizationReceiptKey, PreparedCredentialRotationFacts,
};

use super::{
    admin_adapter::{command, credential},
    *,
};

#[tokio::test]
async fn authorization_persists_an_unnamed_account_with_its_email() {
    let Some(database) = TestDatabase::create("authorization_unnamed").await else {
        return;
    };
    let store = admin_account_store(&database.pool);
    let mut prepared = credential("acct_unnamed", "unnamed-user");
    prepared.name.clear();
    prepared.email = Some("identity@example.com".into());
    let (authorization, context) = command(prepared);
    let result = store
        .commit_authorization(authorization, &context)
        .await
        .unwrap();
    let account = store
        .load_account(
            result.result.account_id.as_str(),
            AccountRuntimeSnapshot::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.account.name, "");
    assert_eq!(
        account.account.email.as_deref(),
        Some("identity@example.com")
    );
    database.close().await;
}

#[tokio::test]
async fn authorization_preserves_saved_proxy_capacity_across_pending_storage() {
    use gateway_core::account::OutboundProxy;

    let Some(database) = TestDatabase::create("authorization_proxy_snapshot").await else {
        return;
    };
    let proxy_url = "socks5h://proxy.example.invalid:1080";
    let proxy_id = "proxy_synthetic";
    sqlx::query(
        "insert into outbound_proxies (id, name, proxy_url, enabled, max_websocket_connections) \
         values ($1, 'Synthetic proxy', $2, true, 16)",
    )
    .bind(proxy_id)
    .bind(proxy_url)
    .execute(&database.pool)
    .await
    .unwrap();
    let (mut authorization, context) = command(credential("acct_proxy_test", "synthetic-user"));
    let pending = authorization
        .pending
        .with_outbound_proxy(Some(
            OutboundProxy::parse(proxy_url)
                .unwrap()
                .with_max_websocket_connections(16),
        ))
        .with_outbound_proxy_id(Some(proxy_id.into()));
    authorization.pending = PendingAuthorizationMutation::from_storage_v1(
        serde_json::Value::Object(pending.to_storage_v1()),
    )
    .unwrap();
    if let AuthorizationCredentialCommit::Create(credential) = &mut authorization.credential {
        credential.outbound_proxy = authorization.pending.outbound_proxy().cloned();
    }
    let store = admin_account_store(&database.pool);
    let committed = store
        .commit_authorization(authorization, &context)
        .await
        .expect("unchanged saved proxy must not cause an authorization conflict");
    let saved_proxy: Option<String> =
        sqlx::query_scalar("select outbound_proxy_id from provider_accounts where id = $1")
            .bind(committed.result.account_id.as_str())
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(saved_proxy.as_deref(), Some(proxy_id));
    database.close().await;
}

#[tokio::test]
async fn authorization_rejects_a_saved_proxy_that_changed_after_start() {
    use gateway_admin::ports::store::AdminStoreErrorKind;
    use gateway_core::account::OutboundProxy;

    let Some(database) = TestDatabase::create("authorization_proxy_changed").await else {
        return;
    };
    let proxy_url = "socks5h://proxy.example.invalid:1080";
    let proxy_id = "proxy_synthetic";
    sqlx::query(
        "insert into outbound_proxies (id, name, proxy_url, enabled, max_websocket_connections) \
         values ($1, 'Synthetic proxy', $2, true, 16)",
    )
    .bind(proxy_id)
    .bind(proxy_url)
    .execute(&database.pool)
    .await
    .unwrap();
    let store = admin_account_store(&database.pool);
    for (current_url, limit, enabled, expected) in [
        (
            "socks5h://other.example.invalid:1080",
            16_i64,
            true,
            AdminStoreErrorKind::StaleRevision,
        ),
        (proxy_url, 17, true, AdminStoreErrorKind::StaleRevision),
        (proxy_url, 16, false, AdminStoreErrorKind::Conflict),
    ] {
        sqlx::query(
            "update outbound_proxies set proxy_url=$2, max_websocket_connections=$3, enabled=$4 where id=$1",
        )
        .bind(proxy_id)
        .bind(current_url)
        .bind(limit)
        .bind(enabled)
        .execute(&database.pool)
        .await
        .unwrap();
        let (mut authorization, context) = command(credential("acct_proxy_test", "synthetic-user"));
        let pending = authorization
            .pending
            .with_outbound_proxy(Some(
                OutboundProxy::parse(proxy_url)
                    .unwrap()
                    .with_max_websocket_connections(16),
            ))
            .with_outbound_proxy_id(Some(proxy_id.into()));
        authorization.pending = PendingAuthorizationMutation::from_storage_v1(
            serde_json::Value::Object(pending.to_storage_v1()),
        )
        .unwrap();
        let receipt_key = authorization.key.clone();
        let error = store
            .commit_authorization(authorization, &context)
            .await
            .expect_err("changed or disabled proxy must fail closed");
        assert_eq!(error.kind(), expected);
        assert!(
            store
                .authorization_receipt(&receipt_key)
                .await
                .unwrap()
                .is_none()
        );
    }
    let accounts: i64 = sqlx::query_scalar("select count(*) from provider_accounts")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(accounts, 0);
    database.close().await;
}

#[tokio::test]
async fn concurrent_authorization_retries_share_one_account_write_and_one_receipt() {
    let Some(database) = TestDatabase::create("authorization_receipt_race").await else {
        return;
    };
    let store = admin_account_store(&database.pool);
    let (authorization, context) = command(credential("acct_receipt", "receipt-user"));
    let key = authorization.key.clone();
    let (first, second) = tokio::join!(
        store.commit_authorization(authorization.clone(), &context),
        store.commit_authorization(authorization, &context)
    );
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_eq!(first.result, second.result);
    assert_ne!(first.newly_committed, second.newly_committed);
    assert_eq!(first.result.credential_revision.unwrap().get(), 1);
    let counts: (i64, i64, i64) = sqlx::query_as("select (select count(*) from provider_accounts), (select count(*) from admin_audit_events where action='authorize'), (select count(*) from provider_authorization_receipts)")
        .fetch_one(&database.pool).await.unwrap();
    assert_eq!(counts, (1, 1, 1));
    let restarted = admin_account_store(&database.pool);
    assert_eq!(
        restarted.authorization_receipt(&key).await.unwrap(),
        Some(first.result.clone())
    );
    let outsider = MutationContext {
        actor: MutationActor::AdminApiKey,
        request_id: "different-owner".into(),
    };
    // 相同 flow 的另一身份不能读取账号结果，也不能另行提交同一个授权
    let (mut foreign, _) = command(credential("acct_foreign", "foreign-user"));
    foreign.key = key.clone();
    assert!(
        restarted
            .commit_authorization(foreign, &outsider)
            .await
            .is_err()
    );
    let stored: String =
        sqlx::query_scalar("select to_jsonb(r)::text from provider_authorization_receipts r")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert!(!stored.contains("access_token") && !stored.contains("test-only"));
    database.close().await;
}

#[tokio::test]
async fn a_receipt_failure_rolls_back_accounts_audit_and_config_revision() {
    let Some(database) = TestDatabase::create("authorization_receipt_failure").await else {
        return;
    };
    let store = admin_account_store(&database.pool);
    let before = current_revision(&database.pool).await;
    sqlx::query(
        "alter table provider_authorization_receipts add constraint fail_receipt check (false)",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    let (command, context) = command(credential("acct_rollback", "rollback-user"));
    assert!(
        store
            .commit_authorization(command.clone(), &context)
            .await
            .is_err()
    );
    assert_eq!(current_revision(&database.pool).await, before);
    let counts: (i64, i64, i64) = sqlx::query_as("select (select count(*) from provider_accounts), (select count(*) from admin_audit_events), (select count(*) from provider_authorization_receipts)").fetch_one(&database.pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
    assert!(
        store
            .authorization_receipt(&command.key)
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("alter table provider_authorization_receipts drop constraint fail_receipt")
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(
        store
            .commit_authorization(command, &context)
            .await
            .unwrap()
            .newly_committed
    );
    database.close().await;
}

#[tokio::test]
async fn expired_receipts_are_unreadable_and_reclaimed_without_removing_accounts() {
    let Some(database) = TestDatabase::create("authorization_receipt_expiry").await else {
        return;
    };
    let store = admin_account_store(&database.pool);
    let (authorization, context) = command(credential("acct_expired", "expired-user"));
    let key = authorization.key.clone();
    store
        .commit_authorization(authorization, &context)
        .await
        .unwrap();
    sqlx::query("update provider_authorization_receipts set created_at=now()-interval '25 hours', expires_at=now()-interval '1 hour'")
        .execute(&database.pool).await.unwrap();
    assert!(store.authorization_receipt(&key).await.unwrap().is_none());
    let (next, context) = command(credential("acct_next", "next-user"));
    let current = next.key.clone();
    let committed = store.commit_authorization(next, &context).await.unwrap();
    assert_eq!(
        store.authorization_receipt(&current).await.unwrap(),
        Some(committed.result)
    );
    let counts: (i64, i64) = sqlx::query_as("select (select count(*) from provider_accounts), (select count(*) from provider_authorization_receipts)")
        .fetch_one(&database.pool).await.unwrap();
    assert_eq!(counts, (2, 1));
    database.close().await;
}

#[tokio::test]
async fn reauthorization_replay_keeps_the_committed_revision_and_later_profile_edits() {
    let Some(database) = TestDatabase::create("reauthorization_receipt").await else {
        return;
    };
    let store = admin_account_store(&database.pool);
    let (seed, context) = command(credential("acct_reauthorize", "reauthorize-user"));
    let receipt = store.commit_authorization(seed, &context).await.unwrap();
    let id = receipt.result.account_id.clone();
    let command = AuthorizationCommit {
        key: AuthorizationReceiptKey::new(
            ProviderKind::new("openai").unwrap(),
            "reauthorize-flow",
            &context,
        )
        .unwrap(),
        pending: PendingAuthorizationMutation::new(
            ProviderKind::new("openai").unwrap(),
            AuthorizationMutationTarget::Reauthorize {
                account_id: id.clone(),
            },
            AuthorizationOwnerBinding::from_context(&context),
        ),
        settings: None,
        credential: AuthorizationCredentialCommit::Reauthorize(Box::new(
            PreparedCredentialRotationFacts {
                account_id: id.clone(),
                provider_kind: ProviderKind::new("openai").unwrap(),
                expected_credential_revision: gateway_admin::model::Revision::new(1).unwrap(),
                replacement_identity: None,
                name: "remote profile".into(),
                email: None,
                plan_type: None,
                preserve_profile: true,
                preserve_credential_state: false,
                provider_material: ProviderDocument::new(OpaqueProviderData::new(
                    json!({"access_token":"rotated-test-only"})
                        .as_object()
                        .unwrap()
                        .clone(),
                )),
                has_refresh_token: false,
                access_token_expires_at: None,
                next_refresh_at: None,
            },
        )),
    };
    let rename = UpdateAccount {
        account_id: id.to_string(),
        name: Some("renamed team".to_owned()),
        notes: None,
        enabled: true,
        concurrency_limit: None,
        weight: gateway_core::account::AccountWeight::DEFAULT,
        model_access: None,
        group_ids: vec![],
        outbound_proxy: None,
    };
    store
        .update_account(rename.clone(), &context)
        .await
        .unwrap();
    let committed = store
        .commit_authorization(command.clone(), &context)
        .await
        .unwrap();
    assert_eq!(
        store
            .load_account(id.as_str(), Default::default())
            .await
            .unwrap()
            .unwrap()
            .account
            .name,
        "renamed team"
    );
    store
        .update_account(
            UpdateAccount {
                name: Some("later profile".to_owned()),
                ..rename
            },
            &context,
        )
        .await
        .unwrap();
    let replayed = store
        .commit_authorization(command.clone(), &context)
        .await
        .unwrap();
    assert!(!replayed.newly_committed);
    assert_eq!(replayed.result, committed.result);
    let account: (String, i64) =
        sqlx::query_as("select name, credential_revision from provider_accounts where id=$1")
            .bind(id.as_str())
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(account, ("later profile".into(), 2));
    let outsider = MutationContext {
        actor: MutationActor::AdminApiKey,
        request_id: "outsider".into(),
    };
    let key = AuthorizationReceiptKey::new(
        ProviderKind::new("openai").unwrap(),
        "reauthorize-flow",
        &outsider,
    )
    .unwrap();
    assert!(store.authorization_receipt(&key).await.is_err());
    let another = AuthorizationReceiptKey::new(
        ProviderKind::new("other").unwrap(),
        "reauthorize-flow",
        &context,
    )
    .unwrap();
    assert!(
        store
            .authorization_receipt(&another)
            .await
            .unwrap()
            .is_none()
    );
    database.close().await;
}
