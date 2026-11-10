//! 用实际 PostgreSQL 工具验收业务备份、恢复与恢复后的统计续算

use chrono::{TimeDelta, Utc};
use gateway_admin::ports::backup::{DatabaseDumpPort as _, DumpRequest};
use gateway_admin::{
    model::client_keys::{
        ClientKeyBudgetMutationOrigin, ClientKeyBudgetWindowPeriod, ExplicitClientKeyBudgetWindow,
        SetClientKeyBudgetWindow,
    },
    ports::store::{AdminStoreErrorKind, ClientKeyStore as _},
};
use gateway_core::engine::budget::ClientBudgetPort as _;
use gateway_core::lifecycle::CancellationToken;
use gateway_store::backup::{pg_dump::PgDumpAdapter, staging::StagingArea};
use gateway_store::postgres::{ObservabilityRange, PgAdminClientKeyStore, PgClientBudgetStore};
use std::{process::Stdio, sync::Arc};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::client_budgets::{charge, context, key_id};
use super::{TestDatabase, analytics::seed, drain_analytics, observability_repository};

#[tokio::test]
async fn business_backup_restores_core_state_and_usage_without_request_history() {
    let Some(database) = TestDatabase::create("business_backup").await else {
        return;
    };
    let hour = "2026-10-15T12:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();
    for id in ["native_a", "native_b"] {
        seed(&database.pool, id, hour + TimeDelta::minutes(5), &[]).await;
    }
    sqlx::raw_sql("insert into client_api_keys(id,name,key,weekly_limit_usd,created_at,updated_at) values('restore_key','restore key','sk_backup_test',9.5,now(),now());
        insert into client_api_key_budget_windows values('restore_key',now(),now()+interval '1 day',now(),now()+interval '7 days',2.5,9.5);
        insert into client_api_key_charge_events(request_id,client_api_key_id,amount_usd,completed_at) values('native_a','restore_key',2.5,now());
        insert into plugin_update_sources(plugin_id,source_json) values('backup.test','{}');
        insert into plugin_artifacts(sha256,plugin_id,version,metadata_json,source_json,archive,accepted_at) values(repeat('a',64),'backup.test','1.0.0','{}','{}',decode('010203','hex'),now());
        update runtime_settings set usage_retention_days=3;
        update model_requests set cost_source='provider_reported',cost_amount=0.5,cost_currency='USD';
        insert into request_analytics_1h(bucket_start,collection,dimensions,outcome,schema_version,histogram_version,counts,costs,distributions)
        values('2026-10-15T12:00:00Z','usage_contribution',array['plugin','model','','','','','maintained'],'',0,0,'{\"request_count\":7,\"total_tokens\":900}','{\"USD\":\"1.25\"}','{}')")
        .execute(&database.pool).await.unwrap();
    drain_analytics(&database.pool).await;
    let range = ObservabilityRange::new(hour, hour + TimeDelta::hours(1)).unwrap();
    let before = observability_repository(&database.pool)
        .usage_summary(range, Default::default())
        .await
        .unwrap();
    assert_eq!(before.requests.request_count, 9);
    assert_eq!(before.requests.total_tokens, 1140);
    assert_eq!(before.attempts.costs[0].currency, "USD");
    assert_eq!(before.attempts.costs[0].amount.as_str(), "2.25");
    let directory = tempfile::tempdir().unwrap();
    let staging =
        Arc::new(StagingArea::open(directory.path().to_owned(), 8 * 1024 * 1024).unwrap());
    let mut url = url::Url::parse(&std::env::var("CPR_TEST_DATABASE_URL").unwrap()).unwrap();
    let password = percent_encoding::percent_decode_str(url.password().unwrap_or_default())
        .decode_utf8()
        .unwrap()
        .into_owned();
    url.set_password(None).unwrap();
    let adapter = PgDumpAdapter::new(
        staging.clone(),
        url.to_string(),
        password.clone(),
        database.pool.clone(),
    );
    sqlx::query("update request_analytics_build_state set backfill_complete=false")
        .execute(&database.pool)
        .await
        .unwrap();
    let error = adapter
        .dump(DumpRequest {
            backup_id: "rebuilding".to_owned(),
            cancellation: CancellationToken::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        gateway_admin::model::backup::code::ANALYTICS_NOT_READY
    );
    assert!(!staging.partial_path("rebuilding").exists());
    sqlx::query("update request_analytics_build_state set backfill_complete=true")
        .execute(&database.pool)
        .await
        .unwrap();
    for (backup_id, expected_requests, expected_tokens) in
        [("first", 9, 1140), ("second", 10, 1260)]
    {
        let artifact = adapter
            .dump(DumpRequest {
                backup_id: backup_id.to_owned(),
                cancellation: CancellationToken::new(),
            })
            .await
            .unwrap();
        let file = tokio::io::BufReader::new(tokio::fs::File::open(&artifact.path).await.unwrap());
        let mut sql = Vec::new();
        async_compression::tokio::bufread::GzipDecoder::new(file)
            .read_to_end(&mut sql)
            .await
            .unwrap();
        let schema: String = sqlx::query_scalar("select current_schema()")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        // 仅清空本测试随机 schema，模拟新库预先存在 public 的状态
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "drop schema \"{schema}\" cascade; create schema \"{schema}\""
        )))
        .execute(&database.pool)
        .await
        .unwrap();
        let mut process = tokio::process::Command::new("psql")
            .args([
                "-X",
                "--single-transaction",
                "--set=ON_ERROR_STOP=1",
                "--dbname",
                url.as_str(),
            ])
            .env("PGPASSWORD", &password)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("PostgreSQL 18 psql must be in PATH");
        process.stdin.take().unwrap().write_all(&sql).await.unwrap();
        let output = process.wait_with_output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let retained: (i64,i64,i64) = sqlx::query_as("select (select count(*) from model_requests),(select count(*) from request_analytics_facts),(select count(*) from client_api_key_charge_events)")
            .fetch_one(&database.pool).await.unwrap();
        assert_eq!(retained, (0, 0, 0));
        let budget: (String,String) = sqlx::query_as("select daily_used_usd::text,weekly_used_usd::text from client_api_key_budget_windows where client_api_key_id='restore_key'")
            .fetch_one(&database.pool).await.unwrap();
        assert_eq!(
            budget,
            ("2.5000000000".to_owned(), "9.5000000000".to_owned())
        );
        let keys = PgAdminClientKeyStore::new(database.pool.clone());
        let ledger = PgClientBudgetStore::new(database.pool.clone());
        let key = key_id("restore_key");
        let saved = keys
            .client_key_budget_window(&key, ClientKeyBudgetWindowPeriod::Weekly)
            .await
            .unwrap();
        let mut change = SetClientKeyBudgetWindow {
            id: key.clone(),
            period: ClientKeyBudgetWindowPeriod::Weekly,
            expected_revision: saved.revision,
            window: Some(ExplicitClientKeyBudgetWindow {
                starts_at: saved.starts_at.unwrap(),
                ends_at: saved.ends_at.unwrap(),
            }),
        };
        let actor = ClientKeyBudgetMutationOrigin::Admin;
        change.expected_revision = keys
            .set_client_key_budget_window(change.clone(), actor.clone(), &context())
            .await
            .unwrap();
        change.window.as_mut().unwrap().ends_at += TimeDelta::microseconds(1);
        let conflict = keys
            .set_client_key_budget_window(change.clone(), actor.clone(), &context())
            .await
            .unwrap_err();
        assert_eq!(conflict.kind(), AdminStoreErrorKind::Conflict);
        assert_eq!(
            ledger
                .admit(key.clone())
                .await
                .unwrap_err()
                .client_error_code(),
            Some("key_weekly_budget_exceeded")
        );
        if backup_id == "second" {
            let start = Utc::now();
            ledger
                .settle(charge("restore_key", "new_cycle", "3"))
                .await
                .unwrap();
            change.window = Some(ExplicitClientKeyBudgetWindow {
                starts_at: start,
                ends_at: start + TimeDelta::hours(1),
            });
            keys.set_client_key_budget_window(change, actor, &context())
                .await
                .unwrap();
            assert_eq!(
                keys.client_key_budget_window(&key, ClientKeyBudgetWindowPeriod::Weekly)
                    .await
                    .unwrap()
                    .used_usd
                    .canonical(),
                "3"
            );
            ledger.admit(key).await.unwrap();
        }
        let archive: Vec<u8> = sqlx::query_scalar(
            "select archive from plugin_artifacts where plugin_id='backup.test'",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert_eq!(archive, [1, 2, 3]);
        let retention_days: i64 =
            sqlx::query_scalar("select usage_retention_days from runtime_settings")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(retention_days, 3);
        drain_analytics(&database.pool).await;
        let restored = observability_repository(&database.pool)
            .usage_summary(range, Default::default())
            .await
            .unwrap();
        assert_eq!(restored.requests.request_count, expected_requests);
        assert_eq!(restored.requests.total_tokens, expected_tokens);
        assert_eq!(restored.attempts.costs, before.attempts.costs);
        if backup_id == "first" {
            seed(
                &database.pool,
                "after_restore",
                hour + TimeDelta::minutes(10),
                &[],
            )
            .await;
            drain_analytics(&database.pool).await;
        }
    }
    database.close().await;
}
