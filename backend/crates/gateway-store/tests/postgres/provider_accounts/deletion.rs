//! 验证大批历史记录下账号删除的原子性、快照保留与审计提交

use super::{
    DeleteProviderAccounts, PgProviderAccountRepository, ProviderAccountAdminRepository,
    ProviderAccountAdminScope, ProviderAccountRepository, TestDatabase, account, account_count,
    audit, current_revision,
};

async fn seed_history(pool: &sqlx::PgPool, account_id: &str, count: i32) {
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, provider_account_id, provider_account_ref, provider_kind,
           operation, client_transport, started_at, deadline_at, outcome, completed_at,
           input_tokens, output_tokens, total_tokens, request_observation_json
         ) select $1 || '_' || n, 'historical_key', $1, $1, 'openai', 'responses', 'http_sse',
           now(), now() + interval '1 hour', 'failed', now(), 10, 5, 15,
           jsonb_build_object(
             'request', jsonb_build_object('configRevision', 1, 'protocol', 'openai',
               'endpoint', '/v1/responses', 'compact', false),
             'account', jsonb_build_object('name', $1::text, 'email', 'test@example.invalid',
               'authenticationKind', 'oauth'),
             'routing', jsonb_build_object('scope', 'all', 'groupRefs', '[]'::jsonb,
               'groupNamesSnapshot', '[]'::jsonb))
           from generate_series(1, $2::integer) n",
    )
    .bind(account_id)
    .bind(count)
    .execute(pool)
    .await
    .expect("seed synthetic history");
}

#[tokio::test]
async fn bulk_account_deletion_preserves_large_history_and_commits_one_revision_and_audit() {
    let Some(db) = TestDatabase::create("account_delete_history").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(db.pool.clone());
    for (id, count) in [("delete_a", 21_421), ("delete_b", 128), ("keep", 3)] {
        repository
            .insert_provider_account(account(id, id))
            .await
            .unwrap();
        seed_history(&db.pool, id, count).await;
    }
    // 保存全部历史字段，只允许本次删除解除选中账号的实时外键引用
    sqlx::query("create table history_before as select * from model_requests")
        .execute(&db.pool)
        .await
        .unwrap();
    let before_revision = current_revision(&db.pool).await;
    let revision = repository
        .delete_provider_accounts_admin(DeleteProviderAccounts {
            scope: ProviderAccountAdminScope {
                provider_kind: "openai".to_owned(),
            },
            account_ids: vec!["delete_a".to_owned(), "delete_b".to_owned()],
            audit: audit("delete_history", "delete", "provider_accounts"),
        })
        .await
        .expect("delete accounts with substantial history");
    assert_eq!(revision.get(), before_revision as u64 + 1);
    for id in ["delete_a", "delete_b"] {
        assert_eq!(account_count(&db.pool, id).await, 0);
    }
    assert_eq!(account_count(&db.pool, "keep").await, 1);
    let changed_history: i64 = sqlx::query_scalar(
        "select count(*) from history_before b full join model_requests r using (id)
         where (to_jsonb(b) - 'provider_account_id') is distinct from
               (to_jsonb(r) - 'provider_account_id')
            or r.provider_account_id is distinct from
               case when b.provider_account_id = 'keep' then 'keep' else null end",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(changed_history, 0);
    let audit_revisions: Vec<i64> = sqlx::query_scalar(
        "select config_revision from admin_audit_events where id = 'delete_history'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(audit_revisions, vec![before_revision + 1]);
    db.close().await;
}

#[tokio::test]
async fn invalid_bulk_deletion_rolls_back_accounts_history_revision_and_audit() {
    let Some(db) = TestDatabase::create("account_delete_rollback").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(db.pool.clone());
    repository
        .insert_provider_account(account("keep", "keep"))
        .await
        .unwrap();
    seed_history(&db.pool, "keep", 3).await;
    let before_revision = current_revision(&db.pool).await;
    repository
        .delete_provider_accounts_admin(DeleteProviderAccounts {
            scope: ProviderAccountAdminScope {
                provider_kind: "openai".to_owned(),
            },
            account_ids: vec!["keep".to_owned(), "missing".to_owned()],
            audit: audit("failed_delete", "delete", "provider_accounts"),
        })
        .await
        .expect_err("one missing account must roll back the entire batch");
    assert_eq!(account_count(&db.pool, "keep").await, 1);
    assert_eq!(current_revision(&db.pool).await, before_revision);
    let retained: i64 = sqlx::query_scalar(
        "select count(*) from model_requests where provider_account_id = 'keep'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(retained, 3);
    let audits: i64 = sqlx::query_scalar("select count(*) from admin_audit_events")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(audits, 0);
    db.close().await;
}
