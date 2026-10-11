//! 持久化统计队列、重复投影、删除修正与筛选口径的数据库验收

use chrono::{DateTime, TimeDelta, Utc};
use gateway_store::postgres::{
    DiagnosticDimension, ObservabilityRange, PgRequestAnalyticsRepository, UsageRecordFilter,
};

use super::{TestDatabase, drain_analytics, observability_query_budget, observability_repository};

pub(super) async fn seed(pool: &sqlx::PgPool, id: &str, start: DateTime<Utc>, groups: &[&str]) {
    let names = groups
        .iter()
        .map(|id| format!("历史 {id}"))
        .collect::<Vec<_>>();
    let observation = serde_json::json!({
        "request": {"configRevision":1,"protocol":"openai","endpoint":"/v1/responses","compact":false,"clientApiKeyNameSnapshot":"历史 Key"},
        "routing": {"scope":if groups.is_empty(){"all"}else{"groups"},"groupRefs":groups,"groupNamesSnapshot":names},
        "timings":{"local":{"latencyMs":100,"firstTokenMs":50}}
    });
    sqlx::query("insert into model_requests(id,client_api_key_ref,operation,client_transport,requested_model_id,outcome,client_status_code,downstream_committed_at,input_tokens,output_tokens,total_tokens,started_at,recovery_lease_expires_at,completed_at,request_observation_json) values($1,'key_analytics','responses','http_sse','model_a','succeeded',200,$2+interval '1 minute',100,20,120,$2,$2+interval '2 minutes',$2+interval '1 minute',$3)")
        .bind(id).bind(start).bind(observation).execute(pool).await.expect("seed analytic request");
}

#[tokio::test]
async fn opaque_identifiers_preserve_long_values_and_response_or_counts_each_request_once() {
    let Some(database) = TestDatabase::create("analytics_opaque_identifiers").await else {
        return;
    };
    let start = Utc::now() - TimeDelta::hours(1);
    seed(&database.pool, "long_response", start, &[]).await;
    seed(&database.pool, "short_response", start, &[]).await;
    // 不可高度压缩的长值同时验证真实源表和投影，不能为索引方便截断 opaque ID
    let opaque: String =
        sqlx::query_scalar("select string_agg(md5(n::text),'') from generate_series(1,256)n")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(opaque.len(), 8192);
    sqlx::query("update model_requests set client_response_id=$1,upstream_response_id=$1,upstream_request_id=$2 where id='long_response'")
        .bind(opaque.as_bytes()).bind(&opaque).execute(&database.pool).await.unwrap();
    sqlx::query("update model_requests set client_response_id=convert_to('short_response','UTF8'),upstream_request_id='short_upstream' where id='short_response'")
        .execute(&database.pool).await.unwrap();
    drain_analytics(&database.pool).await;
    let projected: (Vec<u8>, Vec<u8>, String) = sqlx::query_as("select client_response_id,upstream_response_id,upstream_request_id from request_analytics_facts where id='long_response'")
        .fetch_one(&database.pool).await.unwrap();
    assert_eq!(projected.0, opaque.as_bytes());
    assert_eq!(projected.1, opaque.as_bytes());
    assert_eq!(projected.2, opaque);
    let range =
        ObservabilityRange::new(start - TimeDelta::minutes(1), start + TimeDelta::minutes(1))
            .unwrap();
    let repo = observability_repository(&database.pool);
    for filter in [
        UsageRecordFilter {
            response_id: Some(opaque),
            ..Default::default()
        },
        UsageRecordFilter {
            upstream_request_id: Some("short_upstream".to_owned()),
            ..Default::default()
        },
    ] {
        let result = repo.usage_summary(range, filter).await.unwrap();
        assert_eq!(result.requests.request_count, 1);
        assert_eq!(result.requests.total_tokens, 120);
    }
    database.close().await;
}

#[tokio::test]
async fn projection_replay_late_ops_and_deletion_preserve_exact_contributions() {
    let Some(database) = TestDatabase::create("analytics_lifecycle").await else {
        return;
    };
    let start = DateTime::parse_from_rfc3339("2026-09-01T10:01:00Z")
        .unwrap()
        .with_timezone(&Utc);
    seed(&database.pool, "req_analytics_a", start, &["group_a"]).await;
    let repo = observability_repository(&database.pool);
    let range = ObservabilityRange::new(start - TimeDelta::minutes(1), start + TimeDelta::hours(2))
        .unwrap();
    assert!(
        repo.usage_summary(range, Default::default()).await.is_err(),
        "building must not masquerade as zero"
    );
    drain_analytics(&database.pool).await;
    let original = repo.usage_summary(range, Default::default()).await.unwrap();
    assert_eq!(original.requests.request_count, 1);
    assert_eq!(original.requests.total_tokens, 120);
    assert_eq!(original.attempts.failure_count, 0);
    sqlx::query("insert into ops_events(id,model_request_id,attempt_index,level,component,operation,failure_kind,status_code,message,created_at) values('ops_analytics_late','req_analytics_a',1,'warning','routing','responses','rate_limited',429,'late observation',$1)")
        .bind(start+TimeDelta::seconds(10)).execute(&database.pool).await.unwrap();
    drain_analytics(&database.pool).await;
    let with_ops = repo.usage_summary(range, Default::default()).await.unwrap();
    assert_eq!(with_ops.attempts.failure_count, 1);
    assert_eq!(with_ops.attempts.rate_limited_count, 1);
    sqlx::query("select enqueue_request_analytics('req_analytics_a',$1)")
        .bind(start)
        .execute(&database.pool)
        .await
        .unwrap();
    drain_analytics(&database.pool).await;
    assert_eq!(
        repo.usage_summary(range, Default::default())
            .await
            .unwrap()
            .attempts,
        with_ops.attempts
    );
    sqlx::query("delete from model_requests where id='req_analytics_a'")
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(
        repo.usage_summary(range, Default::default()).await.is_err(),
        "deleted contributions cannot be returned as a new generation"
    );
    drain_analytics(&database.pool).await;
    let deleted = repo.usage_summary(range, Default::default()).await.unwrap();
    assert_eq!(deleted.requests.request_count, 0);
    assert_eq!(deleted.attempts.failure_count, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from request_analytics_15m")
            .fetch_one(&database.pool)
            .await
            .unwrap(),
        0
    );
    database.close().await;
}

#[tokio::test]
async fn rolled_back_source_changes_and_late_commits_do_not_lose_queue_work() {
    let Some(database) = TestDatabase::create("analytics_transactions").await else {
        return;
    };
    let start = Utc::now() - TimeDelta::hours(1);
    seed(&database.pool, "req_analytics_tx", start, &[]).await;
    drain_analytics(&database.pool).await;
    let mut transaction = database.pool.begin().await.unwrap();
    sqlx::query("update model_requests set total_tokens=900 where id='req_analytics_tx'")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    assert_eq!(worker.status().await.unwrap().pending_requests, 0);
    let mut transaction = database.pool.begin().await.unwrap();
    sqlx::query("update model_requests set total_tokens=240 where id='req_analytics_tx'")
        .execute(&mut *transaction)
        .await
        .unwrap();
    // 序列已分配但尚未提交，Worker 不得把最大版本误认为全部处理完成
    worker.maintain(100).await.unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(worker.status().await.unwrap().pending_requests, 1);
    drain_analytics(&database.pool).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "select total_tokens from request_analytics_facts where id='req_analytics_tx'"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap(),
        240
    );
    database.close().await;
}

#[tokio::test]
async fn group_or_filter_counts_each_historical_request_once_and_rebuilds_dirty_buckets() {
    let Some(database) = TestDatabase::create("analytics_groups").await else {
        return;
    };
    let start = DateTime::parse_from_rfc3339("2026-09-01T10:01:00Z")
        .unwrap()
        .with_timezone(&Utc);
    seed(
        &database.pool,
        "req_group_a",
        start,
        &["group_a", "group_b"],
    )
    .await;
    seed(
        &database.pool,
        "req_group_b",
        start + TimeDelta::minutes(1),
        &["group_b"],
    )
    .await;
    drain_analytics(&database.pool).await;
    let repo = observability_repository(&database.pool);
    let range = ObservabilityRange::new(start - TimeDelta::minutes(1), start + TimeDelta::hours(2))
        .unwrap();
    let filter = UsageRecordFilter {
        routing_group_refs: vec!["group_a".into(), "group_b".into()],
        ..Default::default()
    };
    let result = repo.usage_summary(range, filter).await.unwrap();
    assert_eq!(result.requests.request_count, 2);
    assert!(!result.analytics.percentiles_approximate);
    // 已投影但桶待重建时整个桶用事实替换，不能叠加旧桶贡献
    sqlx::query("update request_analytics_facts set total_tokens=333 where id='req_group_a'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) values(date_bin(interval '15 minutes',$1::timestamptz,timestamptz '2000-01-01 UTC'),now())").bind(start).execute(&database.pool).await.unwrap();
    let result = repo.usage_summary(range, Default::default()).await.unwrap();
    assert_eq!(result.requests.request_count, 2);
    assert_eq!(result.requests.total_tokens, 453);
    database.close().await;
}

#[tokio::test]
async fn historical_deletion_does_not_block_disjoint_recent_statistics() {
    let Some(database) = TestDatabase::create("analytics_delete_range").await else {
        return;
    };
    let now = Utc::now();
    seed(
        &database.pool,
        "req_deleted_history",
        now - TimeDelta::days(20),
        &[],
    )
    .await;
    seed(
        &database.pool,
        "req_recent_history",
        now - TimeDelta::hours(1),
        &[],
    )
    .await;
    drain_analytics(&database.pool).await;
    sqlx::query("delete from model_requests where id='req_deleted_history'")
        .execute(&database.pool)
        .await
        .unwrap();
    let repo = observability_repository(&database.pool);
    let recent = ObservabilityRange::new(now - TimeDelta::days(1), now).unwrap();
    assert_eq!(
        repo.usage_summary(recent, Default::default())
            .await
            .unwrap()
            .requests
            .request_count,
        1
    );
    let history = ObservabilityRange::new(now - TimeDelta::days(30), now).unwrap();
    assert!(
        repo.usage_summary(history, Default::default())
            .await
            .is_err()
    );
    drain_analytics(&database.pool).await;
    assert_eq!(
        repo.usage_summary(history, Default::default())
            .await
            .unwrap()
            .requests
            .request_count,
        1
    );
    database.close().await;
}

#[tokio::test]
async fn obsolete_projection_versions_rebuild_from_source_without_double_counting() {
    let Some(database) = TestDatabase::create("analytics_version_rebuild").await else {
        return;
    };
    let now = Utc::now();
    seed(
        &database.pool,
        "req_version_rebuild",
        now - TimeDelta::hours(1),
        &[],
    )
    .await;
    seed(
        &database.pool,
        "req_version_second",
        now - TimeDelta::hours(2),
        &[],
    )
    .await;
    drain_analytics(&database.pool).await;
    sqlx::query("update request_analytics_build_state set schema_version=0,histogram_version=0")
        .execute(&database.pool)
        .await
        .unwrap();
    let repo = observability_repository(&database.pool);
    let range = ObservabilityRange::new(now - TimeDelta::days(1), now).unwrap();
    assert!(repo.usage_summary(range, Default::default()).await.is_err());
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    worker.maintain(1).await.unwrap();
    assert!(worker.status().await.unwrap().building);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from request_analytics_facts")
            .fetch_one(&database.pool)
            .await
            .unwrap(),
        1
    );
    assert!(repo.usage_summary(range, Default::default()).await.is_err());
    // 重建中新的源变更仍排队，销毁 Worker 后由新实例继续已经提交的清理阶段
    sqlx::query("update model_requests set total_tokens=240 where id='req_version_rebuild'")
        .execute(&database.pool)
        .await
        .unwrap();
    drop(worker);
    drain_analytics(&database.pool).await;
    let rebuilt = repo.usage_summary(range, Default::default()).await.unwrap();
    assert_eq!(rebuilt.requests.request_count, 2);
    assert_eq!(rebuilt.requests.total_tokens, 360);
    assert_eq!(rebuilt.analytics.schema_version, 1);
    assert_eq!(rebuilt.analytics.histogram_version, 1);
    database.close().await;
}

#[tokio::test]
async fn high_cardinality_diagnostics_rank_whole_range_before_loading_distributions() {
    let Some(database) = TestDatabase::create("analytics_ranked_groups").await else {
        return;
    };
    drain_analytics(&database.pool).await;
    let start = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    // 超过在线汇总行预算的高基数输入仍只返回全范围排名后的两个组
    sqlx::query("insert into request_analytics_hourly(bucket_start,collection,dimensions,outcome,schema_version,histogram_version,counts,costs,distributions) select $1::timestamptz + hour*interval '1 hour','key',array['key_'||lpad(account::text,4,'0')],'succeeded',1,1,jsonb_build_object('request_count',case when account=1200 then 2 else 1 end),'{}','{}' from generate_series(0,99) hour cross join generate_series(1,1200) account")
        .bind(start).execute(&database.pool).await.unwrap();
    let result = observability_repository(&database.pool)
        .usage_diagnostics(
            ObservabilityRange::new(start, start + TimeDelta::hours(100)).unwrap(),
            UsageRecordFilter::default(),
            DiagnosticDimension::ApiKey,
            2,
        )
        .await
        .unwrap();
    assert_eq!(result.total_request_count, 120100);
    assert_eq!(result.items.len(), 2);
    assert_eq!(result.items[0].key, "key_1200");
    assert_eq!(result.items[0].request_count, 200);
    assert_eq!(result.items[1].key, "key_0001");
    assert_eq!(result.items[1].request_count, 100);
    database.close().await;
}

#[tokio::test]
async fn oversized_bucket_rebuilds_in_segments_without_blocking_other_projection() {
    let Some(database) = TestDatabase::create("analytics_large_bucket").await else {
        return;
    };
    let start = DateTime::parse_from_rfc3339("2026-09-01T10:01:00Z")
        .unwrap()
        .with_timezone(&Utc);
    seed(&database.pool, "req_large_bucket", start, &[]).await;
    drain_analytics(&database.pool).await;
    // 直接扩充派生事实以隔离验证分段重建，不让十万条源触发器掩盖被测路径
    sqlx::query("insert into request_analytics_facts select cloned.* from request_analytics_facts facts cross join generate_series(1,100005) n cross join lateral jsonb_populate_record(null::request_analytics_facts,to_jsonb(facts)||jsonb_build_object('id','large_'||n::text)) cloned where facts.id='req_large_bucket'")
        .execute(&database.pool).await.unwrap();
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) values(date_bin(interval '15 minutes',$1::timestamptz,timestamptz '2000-01-01 UTC'),now())")
        .bind(start).execute(&database.pool).await.unwrap();
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    worker.maintain(1000).await.unwrap();
    worker.maintain(1000).await.unwrap();
    seed(
        &database.pool,
        "req_other_bucket",
        start + TimeDelta::hours(1),
        &[],
    )
    .await;
    let progress = worker.maintain(1000).await.unwrap();
    assert_eq!(
        progress.projected_requests, 1,
        "large dirty buckets must not starve source projection"
    );
    let mut completed = false;
    for _ in 0..120 {
        worker.maintain(1000).await.unwrap();
        if worker.status().await.unwrap().pending_buckets == 0 {
            completed = true;
            break;
        }
    }
    assert!(
        completed,
        "oversized bucket must eventually publish its hour"
    );
    let range = ObservabilityRange::new(start - TimeDelta::minutes(1), start + TimeDelta::hours(2))
        .unwrap();
    let result = observability_repository(&database.pool)
        .usage_summary(range, Default::default())
        .await
        .unwrap();
    assert_eq!(result.requests.request_count, 100007);
    assert_eq!(result.requests.total_tokens, 100007 * 120);
    database.close().await;
}

#[tokio::test]
async fn interrupted_quarters_restart_changed_versions_and_publish_one_consistent_hour() {
    let Some(database) = TestDatabase::create("analytics_staged_versions").await else {
        return;
    };
    let start = DateTime::parse_from_rfc3339("2026-09-01T10:01:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for (id, offset) in [
        ("stage_a", 0),
        ("stage_b", 1),
        ("stage_c", 15),
        ("stage_d", 16),
    ] {
        seed(&database.pool, id, start + TimeDelta::minutes(offset), &[]).await;
    }
    drain_analytics(&database.pool).await;
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) select distinct date_bin(interval '15 minutes',started_at,timestamptz '2000-01-01 UTC'),now() from request_analytics_facts")
        .execute(&database.pool).await.unwrap();
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    for _ in 0..32 {
        worker.maintain(1).await.unwrap();
        let started:bool=sqlx::query_scalar("select exists(select 1 from request_analytics_pending_buckets where build_phase=1 and build_id is not null)").fetch_one(&database.pool).await.unwrap();
        if started {
            break;
        }
    }
    let range = ObservabilityRange::new(start - TimeDelta::minutes(1), start + TimeDelta::hours(1))
        .unwrap();
    let repo = observability_repository(&database.pool);
    assert_eq!(
        repo.usage_summary(range, Default::default())
            .await
            .unwrap()
            .requests
            .request_count,
        4,
        "dirty partial rollups must be ignored"
    );
    sqlx::query("update model_requests set total_tokens=900 where id='stage_a'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("delete from model_requests where id='stage_c'")
        .execute(&database.pool)
        .await
        .unwrap();
    drop(worker);
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    let mut completed = false;
    for _ in 0..120 {
        worker.maintain(1).await.unwrap();
        let status = worker.status().await.unwrap();
        if status.pending_requests == 0 && status.pending_buckets == 0 {
            completed = true;
            break;
        }
    }
    assert!(completed);
    let result = repo.usage_summary(range, Default::default()).await.unwrap();
    assert_eq!(result.requests.request_count, 3);
    assert_eq!(result.requests.total_tokens, 1140);
    let total:i64=sqlx::query_scalar("select sum((counts->>'request_count')::bigint)::bigint from request_analytics_hourly where collection='global'").fetch_one(&database.pool).await.unwrap();
    assert_eq!(total, 3);
    database.close().await;
}

#[tokio::test]
async fn source_updates_do_not_wait_for_blocked_bucket_rebuild() {
    use sqlx::Connection;
    let Some(database) = TestDatabase::create("analytics_source_isolation").await else {
        return;
    };
    let start = Utc::now() - TimeDelta::hours(1);
    seed(&database.pool, "req_no_bucket_lock", start, &[]).await;
    drain_analytics(&database.pool).await;
    sqlx::query("update model_requests set total_tokens=900 where id='req_no_bucket_lock'")
        .execute(&database.pool)
        .await
        .unwrap();
    let mut blocker = sqlx::PgConnection::connect_with(&database.pool.connect_options())
        .await
        .unwrap();
    sqlx::raw_sql("begin; lock table request_analytics_15m in access exclusive mode")
        .execute(&mut blocker)
        .await
        .unwrap();
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    let task = tokio::spawn(async move { worker.maintain(1000).await });
    let mut projected = false;
    for _ in 0..100 {
        let tokens: i64 = sqlx::query_scalar(
            "select total_tokens from request_analytics_facts where id='req_no_bucket_lock'",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        if tokens == 900 {
            projected = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        projected,
        "fact transaction must commit before waiting on aggregation locks"
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        sqlx::query("update model_requests set total_tokens=1200 where id='req_no_bucket_lock'")
            .execute(&database.pool),
    )
    .await
    .expect("source write must not wait for bucket rebuild")
    .unwrap();
    sqlx::query("rollback").execute(&mut blocker).await.unwrap();
    task.await.unwrap().unwrap();
    drain_analytics(&database.pool).await;
    let result = observability_repository(&database.pool)
        .usage_summary(
            ObservabilityRange::new(start - TimeDelta::minutes(1), start + TimeDelta::hours(1))
                .unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(result.requests.total_tokens, 1200);
    database.close().await;
}

#[tokio::test]
async fn backfill_coverage_uses_contiguous_cursor_instead_of_old_corrected_facts() {
    let Some(database) = TestDatabase::create("analytics_partial_coverage").await else {
        return;
    };
    let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    for (id, age) in [
        ("coverage_old", 480),
        ("coverage_gap", 240),
        ("coverage_recent_b", 2),
        ("coverage_recent_a", 1),
    ] {
        seed(&database.pool, id, now - TimeDelta::hours(age), &[]).await;
    }
    // 模拟迁移后历史队列为空，随后最旧请求发生单点修正
    sqlx::query("delete from request_analytics_pending_requests")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("update model_requests set total_tokens=900 where id='coverage_old'")
        .execute(&database.pool)
        .await
        .unwrap();
    let worker =
        PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
    worker.maintain(2).await.unwrap();
    let status = worker.status().await.unwrap();
    assert!(status.building);
    assert_eq!(
        status.coverage_start,
        Some(now - TimeDelta::hours(2) + TimeDelta::microseconds(1))
    );
    let repo = observability_repository(&database.pool);
    let recent = repo
        .usage_summary(
            ObservabilityRange::new(now - TimeDelta::minutes(90), now).unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(recent.requests.request_count, 1);
    assert_eq!(
        recent.analytics.state,
        gateway_admin::model::observability::AnalyticsState::Building
    );
    for range in [
        ObservabilityRange::new(now - TimeDelta::days(30), now - TimeDelta::days(15)).unwrap(),
        ObservabilityRange::new(now - TimeDelta::days(15), now).unwrap(),
    ] {
        assert!(
            repo.usage_summary(range, Default::default()).await.is_err(),
            "a corrected old fact cannot bridge an unprocessed historical gap"
        );
    }
    drain_analytics(&database.pool).await;
    let all = repo
        .usage_summary(
            ObservabilityRange::new(now - TimeDelta::days(30), now).unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(all.requests.request_count, 4);
    assert_eq!(all.requests.total_tokens, 1260);
    database.close().await;
}

#[tokio::test]
async fn clean_aligned_statistics_do_not_access_facts_and_dirty_edges_are_disjoint() {
    use sqlx::Connection;
    let Some(database) = TestDatabase::create("analytics_fact_spans").await else {
        return;
    };
    let start = DateTime::parse_from_rfc3339("2026-09-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for (id, minute) in [
        ("span_before", 2),
        ("span_left", 10),
        ("span_dirty", 20),
        ("span_clean", 40),
    ] {
        seed(&database.pool, id, start + TimeDelta::minutes(minute), &[]).await;
    }
    drain_analytics(&database.pool).await;
    let repo = observability_repository(&database.pool);
    let full = ObservabilityRange::new(start, start + TimeDelta::hours(1)).unwrap();
    let mut blocker = sqlx::PgConnection::connect_with(&database.pool.connect_options())
        .await
        .unwrap();
    sqlx::raw_sql("begin; lock table request_analytics_facts in access exclusive mode")
        .execute(&mut blocker)
        .await
        .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        repo.usage_summary(full, Default::default()),
    )
    .await
    .expect("clean aligned aggregates must not acquire any facts lock")
    .unwrap();
    assert_eq!(result.requests.request_count, 4);
    let diagnostic = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        repo.usage_diagnostics(full, Default::default(), DiagnosticDimension::ApiKey, 2),
    )
    .await
    .expect("aligned TopN must not scan facts")
    .unwrap();
    assert_eq!(diagnostic.total_request_count, 4);
    sqlx::query("rollback").execute(&mut blocker).await.unwrap();
    sqlx::query("update request_analytics_facts set total_tokens=500 where id='span_dirty'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) values($1,now()),($1+interval '15 minutes',now())").bind(start).execute(&database.pool).await.unwrap();
    let partial = ObservabilityRange::new(
        start + TimeDelta::minutes(5),
        start + TimeDelta::minutes(45),
    )
    .unwrap();
    let result = repo
        .usage_summary(partial, Default::default())
        .await
        .unwrap();
    assert_eq!(result.requests.request_count, 3);
    assert_eq!(result.requests.total_tokens, 740);
    let diagnostic = repo
        .usage_diagnostics(partial, Default::default(), DiagnosticDimension::ApiKey, 2)
        .await
        .unwrap();
    assert_eq!(diagnostic.total_request_count, 3);
    assert_eq!(diagnostic.items[0].total_tokens, 740);
    // 与读取器相同的参数化段扫描形状必须保留 started_at 索引条件
    let mut transaction = database.pool.begin().await.unwrap();
    sqlx::query("set local enable_seqscan=off")
        .execute(&mut *transaction)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("explain (analyze,buffers,format json) select fact.id from unnest($1::timestamptz[],$2::timestamptz[]) span(start_at,end_at) cross join lateral (select mr.id from request_analytics_facts mr where mr.started_at>=span.start_at and mr.started_at<span.end_at limit 100001) fact limit 100001")
        .bind(vec![start+TimeDelta::minutes(5),start+TimeDelta::minutes(15)]).bind(vec![start+TimeDelta::minutes(15),start+TimeDelta::minutes(30)])
        .fetch_one(&mut *transaction).await.unwrap();
    let plan = plan.to_string();
    assert!(
        plan.contains("Index Cond")
            && plan.contains("span.start_at")
            && plan.contains("span.end_at"),
        "fact intervals must remain index bounds: {plan}"
    );
    transaction.rollback().await.unwrap();
    database.close().await;
}

#[tokio::test]
async fn persisted_histograms_merge_long_tails_and_distinguish_empty_from_zero() {
    let Some(database) = TestDatabase::create("analytics_histograms").await else {
        return;
    };
    let start = DateTime::parse_from_rfc3339("2026-09-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let samples = [
        0_i64,
        0,
        1,
        2,
        17,
        30_000,
        1_000_000_000,
        1_000_000_000_000_000,
    ];
    for (index, value) in samples.into_iter().enumerate() {
        let id = format!("hist_{index}");
        seed(
            &database.pool,
            &id,
            start + TimeDelta::minutes(index as i64 * 5),
            &["hist_group"],
        )
        .await;
        sqlx::query("update model_requests set request_observation_json=jsonb_set(request_observation_json,'{timings,local}',jsonb_build_object('latencyMs',$2::bigint,'firstTokenMs',0)) where id=$1").bind(id).bind(value).execute(&database.pool).await.unwrap();
    }
    drain_analytics(&database.pool).await;
    let repo = observability_repository(&database.pool);
    let range = ObservabilityRange::new(start, start + TimeDelta::hours(1)).unwrap();
    let approximate = repo.usage_summary(range, Default::default()).await.unwrap();
    let exact = repo
        .usage_summary(
            range,
            UsageRecordFilter {
                routing_group_refs: vec!["hist_group".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(approximate.analytics.percentiles_approximate);
    assert!(!exact.analytics.percentiles_approximate);
    assert_eq!(approximate.requests.latency_count, 8);
    assert_eq!(
        approximate.requests.latency_sum_ms,
        exact.requests.latency_sum_ms
    );
    for (actual, reference) in [
        (
            approximate.requests.latency_percentiles.p50_ms,
            exact.requests.latency_percentiles.p50_ms,
        ),
        (
            approximate.requests.latency_percentiles.p95_ms,
            exact.requests.latency_percentiles.p95_ms,
        ),
        (
            approximate.requests.latency_percentiles.p99_ms,
            exact.requests.latency_percentiles.p99_ms,
        ),
    ] {
        let actual = actual.unwrap().as_f64();
        let reference = reference.unwrap().as_f64();
        assert!((actual - reference).abs() <= reference * 0.03);
    }
    let zero = repo
        .usage_summary(
            ObservabilityRange::new(start, start + TimeDelta::minutes(10)).unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        zero.requests.latency_percentiles.p95_ms.unwrap().as_f64(),
        0.0
    );
    let empty = repo
        .usage_summary(
            ObservabilityRange::new(start + TimeDelta::hours(2), start + TimeDelta::hours(3))
                .unwrap(),
            Default::default(),
        )
        .await
        .unwrap();
    assert!(empty.requests.latency_percentiles.p95_ms.is_none());
    database.close().await;
}

#[tokio::test]
async fn source_deletions_do_not_deadlock_with_worker_publication() {
    use sqlx::Connection;
    for backfill in [false, true] {
        let Some(database) = TestDatabase::create("analytics_delete_publication").await else {
            return;
        };
        let start = Utc::now() - TimeDelta::hours(1);
        seed(&database.pool, "delete_a", start, &[]).await;
        seed(&database.pool, "delete_b", start, &[]).await;
        drain_analytics(&database.pool).await;
        sqlx::query("update model_requests set total_tokens=240 where id='delete_b'")
            .execute(&database.pool)
            .await
            .unwrap();
        if backfill {
            sqlx::query("update request_analytics_build_state set backfill_complete=false,backfill_cursor_started_at=null,backfill_cursor_id=null")
                .execute(&database.pool).await.unwrap();
        }
        let mut source = sqlx::PgConnection::connect_with(&database.pool.connect_options())
            .await
            .unwrap();
        let source_pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
            .fetch_one(&mut source)
            .await
            .unwrap();
        sqlx::raw_sql("begin;delete from model_requests where id='delete_a'")
            .execute(&mut source)
            .await
            .unwrap();
        let worker =
            PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
        let running = tokio::spawn(async move { worker.maintain(10).await });
        // 源事务先持有 generation，确认真实 Worker 到达发布或回填游标的锁等待
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let waiting: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where wait_event_type='Lock' and $1=any(pg_blocking_pids(pid)) and query like 'update request_analytics_build_state%')")
                    .bind(source_pid).fetch_one(&database.pool).await.unwrap();
                if waiting { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("worker reached build-state publication");
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            sqlx::query("delete from model_requests where id='delete_b'").execute(&mut source),
        )
        .await
        .expect("source deletion cannot wait for worker ACK")
        .expect("source deletion cannot be a deadlock victim");
        sqlx::query("commit").execute(&mut source).await.unwrap();
        // RR 事务允许因源提交而重试，但源请求删除不能承受死锁回滚
        let _ = running.await.unwrap();
        drain_analytics(&database.pool).await;
        let remaining: i64 = sqlx::query_scalar("select count(*) from request_analytics_facts")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
        database.close().await;
    }
}

#[tokio::test]
async fn worker_skips_locked_versions_and_preserves_a_concurrent_source_deletion() {
    use sqlx::Connection;
    for backfill in [false, true] {
        let Some(database) = TestDatabase::create("analytics_locked_ack").await else {
            return;
        };
        let start = Utc::now() - TimeDelta::hours(1);
        seed(&database.pool, "locked_ack", start, &[]).await;
        drain_analytics(&database.pool).await;
        sqlx::query("update model_requests set total_tokens=240 where id='locked_ack'")
            .execute(&database.pool)
            .await
            .unwrap();
        if backfill {
            sqlx::query("update request_analytics_build_state set backfill_complete=false,backfill_cursor_started_at=null,backfill_cursor_id=null")
                .execute(&database.pool).await.unwrap();
        }
        let mut source = sqlx::PgConnection::connect_with(&database.pool.connect_options())
            .await
            .unwrap();
        sqlx::raw_sql("begin;update model_requests set total_tokens=360 where id='locked_ack'")
            .execute(&mut source)
            .await
            .unwrap();
        let new_version: i64 = sqlx::query_scalar("select change_version from request_analytics_pending_requests where request_id='locked_ack'")
            .fetch_one(&mut source).await.unwrap();
        let worker =
            PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
        tokio::time::timeout(std::time::Duration::from_secs(2), worker.maintain(10))
            .await
            .expect("ACK must skip the source transaction's row lock")
            .unwrap();
        sqlx::raw_sql("delete from model_requests where id='locked_ack';commit")
            .execute(&mut source)
            .await
            .unwrap();
        let pending: (i64, bool) = sqlx::query_as("select change_version,deletion_pending from request_analytics_pending_requests where request_id='locked_ack'")
            .fetch_one(&database.pool).await.expect("new source version cannot be acknowledged by old projection");
        assert!(pending.0 > new_version);
        assert!(pending.1);
        drain_analytics(&database.pool).await;
        assert_eq!(worker.status().await.unwrap().pending_requests, 0);
        let remaining: i64 = sqlx::query_scalar("select count(*) from request_analytics_facts")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
        database.close().await;
    }
}

#[tokio::test]
async fn committed_queue_versions_abort_old_ack_and_replay_without_losing_contributions() {
    use sqlx::Connection;
    for backfill in [false, true] {
        let Some(database) = TestDatabase::create("analytics_committed_ack").await else {
            return;
        };
        let start = Utc::now() - TimeDelta::hours(1);
        seed(&database.pool, "committed_ack", start, &[]).await;
        drain_analytics(&database.pool).await;
        sqlx::query("update model_requests set total_tokens=240 where id='committed_ack'")
            .execute(&database.pool)
            .await
            .unwrap();
        if backfill {
            sqlx::query("update request_analytics_build_state set backfill_complete=false,backfill_cursor_started_at=null,backfill_cursor_id=null")
                .execute(&database.pool).await.unwrap();
        }
        let mut publication = sqlx::PgConnection::connect_with(&database.pool.connect_options())
            .await
            .unwrap();
        let publication_pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
            .fetch_one(&mut publication)
            .await
            .unwrap();
        // 只锁发布行、不修改版本，让 Worker 建立旧快照后停在 ACK 之前
        sqlx::raw_sql("begin;select singleton from request_analytics_build_state for update")
            .execute(&mut publication)
            .await
            .unwrap();
        let worker =
            PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
        let running = tokio::spawn(async move { worker.maintain(10).await });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let waiting: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where wait_event_type='Lock' and $1=any(pg_blocking_pids(pid)) and query like 'update request_analytics_build_state%')")
                    .bind(publication_pid).fetch_one(&database.pool).await.unwrap();
                if waiting { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("worker reached publication with its old queue snapshot");
        // 源事务在 ACK 前已经提交，区别于 SKIP LOCKED 可以跳过的未提交版本
        sqlx::query("update model_requests set total_tokens=360 where id='committed_ack'")
            .execute(&database.pool)
            .await
            .unwrap();
        let committed_version: i64 = sqlx::query_scalar("select change_version from request_analytics_pending_requests where request_id='committed_ack'")
            .fetch_one(&database.pool).await.unwrap();
        sqlx::query("commit")
            .execute(&mut publication)
            .await
            .unwrap();
        let error = running
            .await
            .unwrap()
            .expect_err("RR cannot acknowledge a queue row changed after its snapshot");
        let gateway_store::StoreError::Unavailable {
            message,
            source: Some(source),
            ..
        } = error
        else {
            panic!("expected a PostgreSQL serialization failure");
        };
        assert_eq!(message, "confirm analytics request versions");
        assert_eq!(
            source
                .downcast_ref::<sqlx::Error>()
                .expect("preserve original sqlx error")
                .as_database_error()
                .expect("preserve PostgreSQL error")
                .code()
                .as_deref(),
            Some("40001")
        );
        let preserved: (i64, i64) = sqlx::query_as("select pending.change_version,facts.total_tokens from request_analytics_pending_requests pending join request_analytics_facts facts on facts.id=pending.request_id where pending.request_id='committed_ack'")
            .fetch_one(&database.pool).await.unwrap();
        assert_eq!(preserved, (committed_version, 120));
        drain_analytics(&database.pool).await;
        let summary = observability_repository(&database.pool)
            .usage_summary(
                ObservabilityRange::new(
                    start - TimeDelta::minutes(1),
                    start + TimeDelta::minutes(1),
                )
                .unwrap(),
                Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(summary.requests.request_count, 1);
        assert_eq!(summary.requests.total_tokens, 360);
        database.close().await;
    }
}

#[tokio::test]
async fn parent_cascades_invalidate_once_and_explicit_ops_mutations_keep_their_contribution() {
    let Some(database) = TestDatabase::create("analytics_cascade_generation").await else {
        return;
    };
    let start = Utc::now() - TimeDelta::hours(1);
    for id in ["cascade_a", "cascade_b", "ops_survivor"] {
        seed(&database.pool, id, start, &[]).await;
        sqlx::query("insert into ops_events(id,model_request_id,attempt_index,level,component,operation,failure_kind,status_code,message,created_at) values($1,$2,1,'warning','routing','responses','rate_limited',429,'synthetic',$3)")
            .bind(format!("event_{id}")).bind(id).bind(start).execute(&database.pool).await.unwrap();
    }
    drain_analytics(&database.pool).await;
    let before: i64 = sqlx::query_scalar("select generation from request_analytics_build_state")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    sqlx::query("delete from model_requests where id in ('cascade_a','cascade_b')")
        .execute(&database.pool)
        .await
        .unwrap();
    let after: i64 = sqlx::query_scalar("select generation from request_analytics_build_state")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        after,
        before + 1,
        "the parent statement owns cascade invalidation"
    );
    drain_analytics(&database.pool).await;
    sqlx::query("update ops_events set failure_kind='authentication',status_code=401 where id='event_ops_survivor'")
        .execute(&database.pool).await.unwrap();
    drain_analytics(&database.pool).await;
    let repo = observability_repository(&database.pool);
    let range =
        ObservabilityRange::new(start - TimeDelta::minutes(1), start + TimeDelta::minutes(5))
            .unwrap();
    let changed = repo.usage_summary(range, Default::default()).await.unwrap();
    assert_eq!(changed.attempts.auth_failure_count, 1);
    assert_eq!(changed.attempts.rate_limited_count, 0);
    let before: i64 = sqlx::query_scalar("select generation from request_analytics_build_state")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    sqlx::query("delete from ops_events where id='event_ops_survivor'")
        .execute(&database.pool)
        .await
        .unwrap();
    let after: i64 = sqlx::query_scalar("select generation from request_analytics_build_state")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(after, before + 1);
    assert!(repo.usage_summary(range, Default::default()).await.is_err());
    drain_analytics(&database.pool).await;
    let deleted = repo.usage_summary(range, Default::default()).await.unwrap();
    assert_eq!(deleted.requests.request_count, 1);
    assert_eq!(deleted.attempts.failure_count, 0);
    database.close().await;
}

#[tokio::test]
async fn source_move_then_delete_hides_the_still_projected_old_bucket() {
    assert_moved_deletion_is_hidden(false).await;
}

#[tokio::test]
async fn unacknowledged_move_then_delete_hides_the_current_fact_bucket() {
    assert_moved_deletion_is_hidden(true).await;
}

async fn assert_moved_deletion_is_hidden(project_without_ack: bool) {
    let Some(database) = TestDatabase::create("analytics_moved_delete").await else {
        return;
    };
    let old = DateTime::parse_from_rfc3339("2026-09-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let moved = old - TimeDelta::hours(1);
    seed(
        &database.pool,
        "moved_delete",
        old + TimeDelta::minutes(1),
        &[],
    )
    .await;
    drain_analytics(&database.pool).await;
    if project_without_ack {
        // 第一条待处理版本记录 A，随后源时间移到 B，队列仍保留最早的桶
        sqlx::query("update model_requests set total_tokens=240 where id='moved_delete'")
            .execute(&database.pool)
            .await
            .unwrap();
    }
    sqlx::query("update model_requests set started_at=$1 where id='moved_delete'")
        .bind(moved + TimeDelta::minutes(1))
        .execute(&database.pool)
        .await
        .unwrap();
    if project_without_ack {
        let mut lock = database.pool.begin().await.unwrap();
        sqlx::query("select request_id from request_analytics_pending_requests where request_id='moved_delete' for update")
            .fetch_one(&mut *lock).await.unwrap();
        let worker =
            PgRequestAnalyticsRepository::new(database.pool.clone(), observability_query_budget());
        worker.maintain(10).await.unwrap();
        lock.commit().await.unwrap();
    }
    let projected: DateTime<Utc> = sqlx::query_scalar(
        "select started_at from request_analytics_facts where id='moved_delete'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    let previous: DateTime<Utc> = sqlx::query_scalar("select previous_bucket from request_analytics_pending_requests where request_id='moved_delete'")
        .fetch_one(&database.pool).await.unwrap();
    let visible_bucket = if project_without_ack { moved } else { old };
    assert_eq!(projected, visible_bucket + TimeDelta::minutes(1));
    assert_eq!(previous, if project_without_ack { old } else { moved });
    sqlx::query("delete from model_requests where id='moved_delete'")
        .execute(&database.pool)
        .await
        .unwrap();
    let repo = observability_repository(&database.pool);
    let range =
        ObservabilityRange::new(visible_bucket, visible_bucket + TimeDelta::minutes(15)).unwrap();
    assert!(
        repo.usage_summary(range, Default::default()).await.is_err(),
        "a source deletion must hide the actually projected bucket even when the queue remembers another bucket"
    );
    drain_analytics(&database.pool).await;
    for start in [old, moved] {
        let range = ObservabilityRange::new(start, start + TimeDelta::minutes(15)).unwrap();
        assert_eq!(
            repo.usage_summary(range, Default::default())
                .await
                .unwrap()
                .requests
                .request_count,
            0
        );
    }
    database.close().await;
}

#[tokio::test]
async fn ranked_buckets_preserve_top100_denominators_filters_and_dirty_boundaries() {
    let Some(database) = TestDatabase::create("analytics_ranked_bucket_boundaries").await else {
        return;
    };
    let start = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    seed(
        &database.pool,
        "rank_template",
        start - TimeDelta::days(1),
        &[],
    )
    .await;
    sqlx::query("insert into model_requests select cloned.* from model_requests template cross join generate_series(1,120) entity cross join unnest(array[-5,5,20,35,50,65,80,95,110,125,140,155,170,185]) minute cross join lateral generate_series(1,entity%4+1) copy cross join lateral jsonb_populate_record(null::model_requests,to_jsonb(template)||jsonb_build_object('id','rank_'||entity||'_'||minute||'_'||copy,'client_api_key_ref','key_rank_'||lpad(entity::text,4,'0'),'provider_account_ref','account_rank_'||lpad(entity::text,4,'0'),'started_at',$1::timestamptz + minute*interval '1 minute','completed_at',$1::timestamptz+(minute+1)*interval '1 minute','recovery_lease_expires_at',$1::timestamptz+(minute+2)*interval '1 minute','downstream_committed_at',$1::timestamptz+(minute+1)*interval '1 minute','outcome',case when minute in(20,35,80,140) then 'failed' else 'succeeded' end,'client_status_code',case when minute in(20,35,80,140) then 500 else 200 end,'recovered_at',case when minute=35 then $1::timestamptz+interval '1 day' end,'recovery_request_id',case when minute=35 then 'rank_template' end,'recovery_attempt_count',case when minute=35 then 1 else 0 end,'request_observation_json',template.request_observation_json || case when minute=35 then jsonb_build_object('recovery',jsonb_build_object('retryDelayMs',0,'totalLatencyMs',1)) else '{}'::jsonb end)) cloned where template.id='rank_template'")
        .bind(start).execute(&database.pool).await.unwrap();
    drain_analytics(&database.pool).await;
    // 同一小时的两个 dirty quarter 必须回退事实，另一个完整干净小时仍使用 hour 汇总
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) values($1,now()),($1+interval '1 hour',now()),($1+interval '90 minutes',now())")
        .bind(start).execute(&database.pool).await.unwrap();
    let range = ObservabilityRange::new(
        start + TimeDelta::minutes(5),
        start + TimeDelta::minutes(185),
    )
    .unwrap();
    let repo = observability_repository(&database.pool);
    for dimension in [DiagnosticDimension::ApiKey, DiagnosticDimension::Account] {
        let column = if dimension == DiagnosticDimension::ApiKey {
            "client_api_key_ref"
        } else {
            "provider_account_ref"
        };
        for outcome in [None, Some("failed")] {
            let filter = UsageRecordFilter {
                outcome: outcome.map(str::to_owned),
                ..Default::default()
            };
            let result = repo
                .usage_diagnostics(range, filter, dimension, 100)
                .await
                .unwrap();
            // 请求分母包含失败，Token 只累计本用例中已成功完整交付的用量事实
            let statement = format!(
                "select {column},count(*)::bigint,sum(case when outcome='succeeded' then total_tokens else 0 end)::bigint,sum(count(*)) over()::bigint from request_analytics_facts where started_at >= $1 and started_at < $2 and recovered_at is null and ($3::text is null or outcome=$3) group by {column} order by count(*) desc,{column} limit 100"
            );
            let expected: Vec<(String, i64, i64, i64)> =
                sqlx::query_as(sqlx::AssertSqlSafe(statement))
                    .bind(range.start)
                    .bind(range.end)
                    .bind(outcome)
                    .fetch_all(&database.pool)
                    .await
                    .unwrap();
            assert_eq!(result.items.len(), 100);
            assert_eq!(result.total_request_count, expected[0].3 as u64);
            for (actual, expected) in result.items.iter().zip(&expected) {
                assert_eq!(
                    (&actual.key, actual.request_count, actual.total_tokens),
                    (&expected.0, expected.1 as u64, expected.2 as u64)
                );
            }
        }
    }
    database.close().await;
}

#[tokio::test]
async fn clean_complete_hours_do_not_wait_for_unused_quarter_storage() {
    let Some(database) = TestDatabase::create("analytics_unused_quarters").await else {
        return;
    };
    let start = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    seed(
        &database.pool,
        "hour_only",
        start + TimeDelta::minutes(1),
        &[],
    )
    .await;
    drain_analytics(&database.pool).await;
    let range = ObservabilityRange::new(start, start + TimeDelta::hours(1)).unwrap();
    let mut blocker = database.pool.begin().await.unwrap();
    // 模拟另一个事务维护不需要的十五分钟表，完整小时查询不应等待该表的锁
    sqlx::query("lock table request_analytics_15m in access exclusive mode")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let repo = observability_repository(&database.pool);
    let summary = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        repo.usage_summary(range, Default::default()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(summary.requests.request_count, 1);
    let diagnostics = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        repo.usage_diagnostics(range, Default::default(), DiagnosticDimension::ApiKey, 100),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(diagnostics.total_request_count, 1);
    assert_eq!(diagnostics.items[0].request_count, 1);
    blocker.rollback().await.unwrap();
    database.close().await;
}

#[tokio::test]
async fn daily_rank_counts_follow_rebuilds_rollbacks_deletions_and_dirty_days() {
    let Some(database) = TestDatabase::create("analytics_daily_rank_counts").await else {
        return;
    };
    let day = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for (id, minutes) in [("daily_a", 1), ("daily_b", 721), ("daily_next", 1441)] {
        seed(&database.pool, id, day + TimeDelta::minutes(minutes), &[]).await;
    }
    drain_analytics(&database.pool).await;
    let totals: Vec<i64> = sqlx::query_scalar("select request_count from request_analytics_daily_counts where collection='key' order by bucket_start")
        .fetch_all(&database.pool).await.unwrap();
    assert_eq!(totals, vec![2, 1]);
    let repo = observability_repository(&database.pool);
    let range = ObservabilityRange::new(day, day + TimeDelta::days(1)).unwrap();
    assert_eq!(
        repo.usage_diagnostics(range, Default::default(), DiagnosticDimension::ApiKey, 100)
            .await
            .unwrap()
            .total_request_count,
        2
    );
    // 同值重放与事务回滚不能改变日计数，小时数据与派生计数共同提交
    sqlx::query("update request_analytics_hourly set counts=counts where collection='key'")
        .execute(&database.pool)
        .await
        .unwrap();
    let mut rollback = database.pool.begin().await.unwrap();
    sqlx::query("delete from request_analytics_hourly where collection='key'")
        .execute(&mut *rollback)
        .await
        .unwrap();
    rollback.rollback().await.unwrap();
    let after: Vec<i64> = sqlx::query_scalar("select request_count from request_analytics_daily_counts where collection='key' order by bucket_start")
        .fetch_all(&database.pool).await.unwrap();
    assert_eq!(after, totals);
    // 删除最后一条该日贡献时，派生行也随小时重建移除
    sqlx::query("delete from model_requests where id='daily_next'")
        .execute(&database.pool)
        .await
        .unwrap();
    drain_analytics(&database.pool).await;
    let rows: i64 = sqlx::query_scalar(
        "select count(*) from request_analytics_daily_counts where collection='key'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(rows, 1);
    // 模拟未发布日状态与任意 dirty quarter，查询必须从干净小时和事实重建完整分母
    sqlx::query(
        "update request_analytics_daily_counts set request_count=999999 where collection='key'",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) values($1,now())")
        .bind(day).execute(&database.pool).await.unwrap();
    let dirty = repo
        .usage_diagnostics(range, Default::default(), DiagnosticDimension::ApiKey, 100)
        .await
        .unwrap();
    assert_eq!(dirty.total_request_count, 2);
    assert_eq!(dirty.items[0].request_count, 2);
    database.close().await;
}
