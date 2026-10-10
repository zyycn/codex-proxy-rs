//! 观测存储测试入口，以及查询范围、过滤与端口合同测试

mod projection;

use futures::TryStreamExt;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use chrono::{TimeDelta, Utc};
use futures::future::BoxFuture;
use gateway_admin::{
    model::observability as admin_observability,
    ports::store::ObservabilityStore as AdminObservabilityStore,
};
use gateway_core::{
    account::{CredentialRevision, ProviderAccountId},
    provider_ports::{
        ProviderCooldown, ProviderCooldownPort, ProviderCooldownScope, ProviderScopedCooldown,
        ProviderStoreError,
    },
};
use gateway_store::postgres::{
    DiagnosticDimension, ObservabilityPageSize, ObservabilityRange, OpsErrorFilter, OpsErrorQuery,
    PgObservabilityRepository, ProviderAccountUsageQuery, UsageRecordFilter, UsageRecordQuery,
};
use sqlx::PgPool;

use super::{
    TestDatabase, admin_observability_store, observability_query_budget, observability_repository,
};

#[test]
fn observability_range_rejects_empty_window() {
    let now = Utc::now();
    assert!(ObservabilityRange::new(now, now).is_err());
}

#[test]
fn observability_range_accepts_full_configured_retention_window() {
    let now = Utc::now();
    let range = ObservabilityRange::new(now - TimeDelta::days(730), now)
        .expect("store range must not truncate configured retention");

    assert_eq!(range.start, now - TimeDelta::days(730));
    assert_eq!(range.end, now);
}

#[test]
fn usage_outcome_filter_should_accept_bounded_unknown_values() {
    assert!(
        UsageRecordFilter {
            outcome: Some("provider_future_state".to_owned()),
            ..UsageRecordFilter::default()
        }
        .validate()
        .is_ok()
    );
    assert!(
        UsageRecordFilter {
            outcome: Some("a".repeat(257)),
            ..UsageRecordFilter::default()
        }
        .validate()
        .is_err()
    );
}

#[tokio::test]
async fn output_throughput_uses_official_response_duration_without_local_fallback() {
    let Some(database) = TestDatabase::create("output_throughput_official_duration").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now).await.unwrap();
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");
    let repository = observability_repository(&database.pool);
    let store = admin_observability_store(&database.pool);
    let admin_range = admin_observability::TimeRange::new(range.start, range.end).unwrap();

    // 官方输出已包含推理量；本地总耗时和首字不能改变速率，也不能补齐上游耗时
    for (first_token, latency, upstream, output, expected) in [
        (
            Some(14_238_i64),
            Some(15_604_i64),
            Some(7_000_i64),
            Some(112_i64),
            Some(16),
        ),
        (None, Some(15_604), Some(7_000), Some(112), Some(16)),
        (Some(15_603), Some(15_604), Some(7_000), Some(112), Some(16)),
        (None, None, Some(7_000), Some(112), Some(16)),
        (None, Some(0), Some(7_000), Some(112), Some(16)),
        (Some(10_196), Some(11_186), Some(3_000), Some(46), Some(15)),
        (None, Some(15_604), None, Some(112), None),
        (None, Some(15_604), Some(7_000), Some(0), None),
        (None, Some(15_604), Some(7_000), None, None),
    ] {
        sqlx::query(
            "update model_requests
             set output_tokens = $1,
           reasoning_tokens = case when $1 > 0 then 20 else null end,
           request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'timings', coalesce(request_observation_json #> '{timings}', '{}'::jsonb) || jsonb_build_object(
             'local', coalesce(request_observation_json #> '{timings,local}', '{}'::jsonb) || jsonb_build_object(
               'firstTokenMs', $2::bigint,
               'latencyMs', $3::bigint),
             'upstream', coalesce(request_observation_json #> '{timings,upstream}', '{}'::jsonb) || jsonb_build_object(
               'responseMs', $4::bigint, 'engineIapiTbtMs', 2.450638))))
             where id = 'req_observe_success'",
        )
        .bind(output)
        .bind(first_token)
        .bind(latency)
        .bind(upstream)
        .execute(&database.pool)
        .await
        .unwrap();

        let records = repository
            .list_usage_records(UsageRecordQuery {
                range,
                filter: UsageRecordFilter::default(),
                current_page: 1,
                page_size: ObservabilityPageSize::new(10).unwrap(),
            })
            .await
            .expect("official duration in usage list");
        let record = records
            .items
            .iter()
            .find(|record| record.id == "req_observe_success")
            .unwrap();
        assert_eq!(
            record.upstream_response_ms,
            upstream.map(|value| value as u64)
        );
        let detail = repository
            .usage_record_detail("req_observe_success")
            .await
            .expect("official duration in usage detail");
        assert_eq!(
            detail.request.upstream_response_ms,
            record.upstream_response_ms
        );
        assert_eq!(record.upstream_engine_iapi_tbt_ms, Some(2.450638));
        assert_eq!(detail.request.upstream_engine_iapi_tbt_ms, Some(2.450638));

        let summary = repository
            .usage_summary(range, UsageRecordFilter::default())
            .await
            .expect("throughput summary");
        assert_eq!(summary.requests.output_throughput_p10, expected);
        assert_eq!(summary.requests.output_throughput_p50, expected);
        assert_eq!(summary.requests.output_throughput_p90, expected);

        let usage = store
            .usage_trend(
                admin_range,
                admin_observability::UsageFilter::default(),
                admin_observability::Granularity::FifteenMinutes,
            )
            .await
            .expect("usage throughput trend");
        let dashboard = store
            .dashboard_trend(
                admin_range,
                admin_observability::Granularity::FifteenMinutes,
            )
            .await
            .expect("dashboard throughput trend");
        for trend in [usage, dashboard] {
            for value in [
                trend
                    .iter()
                    .find_map(|point| point.metrics.output_throughput_p10),
                trend
                    .iter()
                    .find_map(|point| point.metrics.output_throughput_p50),
                trend
                    .iter()
                    .find_map(|point| point.metrics.output_throughput_p90),
            ] {
                assert_eq!(value, expected);
            }
        }
    }

    database.close().await;
}

#[tokio::test]
async fn observability_preserves_and_filters_opaque_response_ids() {
    let Some(database) = TestDatabase::create("observability_opaque_response_id").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let response_id = format!("resp_{}\0opaque", "x".repeat(4_096));
    sqlx::query(
        "update model_requests
         set client_response_id = $1, upstream_response_id = $1
         where id = 'req_observe_success'",
    )
    .bind(response_id.as_bytes().to_vec())
    .execute(&database.pool)
    .await
    .expect("persist opaque response IDs");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");
    let repository = observability_repository(&database.pool);

    let records = repository
        .list_usage_records(UsageRecordQuery {
            range,
            filter: UsageRecordFilter {
                response_id: Some(response_id.clone()),
                ..UsageRecordFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("filter opaque response ID");
    assert_eq!(records.total, 1);
    assert_eq!(records.items[0].id, "req_observe_success");
    let detail = repository
        .usage_record_detail("req_observe_success")
        .await
        .expect("usage detail with opaque response IDs");
    assert_eq!(
        detail.request.client_response_id.as_deref(),
        Some(response_id.as_str())
    );
    assert_eq!(
        detail.request.upstream_response_id.as_deref(),
        Some(response_id.as_str())
    );

    database.close().await;
}

#[tokio::test]
async fn usage_page_should_always_return_total() {
    let Some(database) = TestDatabase::create("usage_page_with_total").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");

    let page = observability_repository(&database.pool)
        .list_usage_records(UsageRecordQuery {
            range,
            filter: UsageRecordFilter::default(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("usage page with total");

    assert_eq!(page.total, 1);
    assert_eq!(page.current_page, 1);
    assert_eq!(page.page_size, 10);
    assert_eq!(page.items[0].client_api_key_name, None);
    database.close().await;
}

#[tokio::test]
async fn usage_list_and_diagnostics_should_resolve_current_account_metadata_by_id() {
    let Some(database) = TestDatabase::create("usage_account_notes").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, email, notes, upstream_user_id,
           upstream_account_id, plan_type, authentication_kind,
           provider_credentials_json, credential_revision, credential_observed_at,
           has_refresh_token, created_at, updated_at
         ) select 'acct_team', provider_kind, name, email, '团队工作区', upstream_user_id,
                  'team-workspace', 'team', authentication_kind,
                  provider_credentials_json, credential_revision, credential_observed_at,
                  has_refresh_token, created_at, updated_at
           from provider_accounts where id = 'acct_observe'",
    )
    .execute(&database.pool)
    .await
    .expect("seed same-email team account");
    sqlx::query(
        "update model_requests
         set provider_account_id = 'acct_team', provider_account_ref = 'acct_team',
             downstream_committed_at = completed_at
         where id = 'req_observe_uncommitted'",
    )
    .execute(&database.pool)
    .await
    .expect("assign completed request to team account");
    let range =
        admin_observability::TimeRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
            .expect("admin observability range");
    let store = admin_observability_store(&database.pool);

    for (notes, plan) in [
        (None, Some("pro")),
        (Some("个人主号"), Some("plus")),
        (Some("个人备用号"), Some("pro")),
        (None, None),
    ] {
        sqlx::query(
            "update provider_accounts set notes = $1, plan_type = $2 where id = 'acct_observe'",
        )
        .bind(notes)
        .bind(plan)
        .execute(&database.pool)
        .await
        .expect("update current account notes");
        let page = store
            .list_usage_records(admin_observability::UsageQuery {
                range,
                filter: admin_observability::UsageFilter::default(),
                current_page: 1,
                page_size: ObservabilityPageSize::new(10).expect("page size"),
            })
            .await
            .expect("usage list with current notes");
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 2);
        let personal = page
            .items
            .iter()
            .find(|record| record.id == "req_observe_success")
            .expect("personal request");
        let team = page
            .items
            .iter()
            .find(|record| record.id == "req_observe_uncommitted")
            .expect("team request");
        assert_eq!(
            personal.provider_account_ref.as_deref(),
            Some("acct_observe")
        );
        assert_eq!(personal.provider_account_email, team.provider_account_email);
        assert_eq!(personal.provider_account_notes.as_deref(), notes);
        assert_eq!(personal.provider_account_plan_type.as_deref(), plan);
        assert_eq!(team.provider_account_plan_type.as_deref(), Some("team"));
        let diagnostics = store
            .usage_diagnostics(
                range,
                admin_observability::UsageFilter::default(),
                admin_observability::DiagnosticDimension::Account,
                100,
            )
            .await
            .expect("account plans");
        assert_eq!(
            diagnostics
                .items
                .iter()
                .find(|item| item.key == "acct_observe")
                .expect("personal diagnostics")
                .account_plan_type
                .as_deref(),
            plan
        );
        assert_eq!(
            diagnostics
                .items
                .iter()
                .find(|item| item.key == "acct_team")
                .expect("team diagnostics")
                .account_plan_type
                .as_deref(),
            Some("team")
        );
        assert_eq!(team.provider_account_notes.as_deref(), Some("团队工作区"));
    }

    let dashboard = store
        .dashboard_summary(
            admin_observability::DashboardQuery::new(admin_observability::TimeRange {
                start: range.start,
                end: range.end,
            }),
            now,
        )
        .await
        .expect("dashboard with current account notes");
    assert_eq!(
        dashboard
            .recent_requests
            .iter()
            .find(|record| record.provider_account_ref.as_deref() == Some("acct_team"))
            .expect("team dashboard request")
            .provider_account_notes
            .as_deref(),
        Some("团队工作区")
    );

    sqlx::query("delete from provider_accounts where id = 'acct_team'")
        .execute(&database.pool)
        .await
        .expect("delete team account");
    let page = store
        .list_usage_records(admin_observability::UsageQuery {
            range,
            filter: admin_observability::UsageFilter {
                provider_account_ref: Some("acct_team".to_owned()),
                ..admin_observability::UsageFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("deleted account history");
    assert_eq!(page.total, 1);
    assert_eq!(
        page.items[0].provider_account_ref.as_deref(),
        Some("acct_team")
    );
    assert_eq!(
        page.items[0].provider_account_email.as_deref(),
        Some("account@example.invalid")
    );
    assert_eq!(page.items[0].provider_account_notes, None);
    assert_eq!(page.items[0].provider_account_plan_type, None);
    let diagnostics = store
        .usage_diagnostics(
            range,
            admin_observability::UsageFilter {
                provider_account_ref: Some("acct_team".to_owned()),
                ..Default::default()
            },
            admin_observability::DiagnosticDimension::Account,
            100,
        )
        .await
        .expect("deleted account diagnostics");
    assert_eq!(diagnostics.items[0].account_plan_type, None);
    database.close().await;
}

#[tokio::test]
async fn usage_search_should_match_literal_prefix_instead_of_substring() {
    let Some(database) = TestDatabase::create("usage_literal_prefix_search").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");

    let page = observability_repository(&database.pool)
        .list_usage_records(UsageRecordQuery {
            range,
            filter: UsageRecordFilter {
                search: Some("observe_success".to_owned()),
                ..UsageRecordFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("usage substring search");

    assert_eq!(page.total, 0);
    database.close().await;
}

#[tokio::test]
async fn usage_search_should_match_account_email_and_name_prefixes() {
    let Some(database) = TestDatabase::create("usage_account_prefix").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");

    for search in ["account@example.invalid", "account@", "primary", "pri"] {
        assert_usage_search_ids(&database.pool, range, search, &["req_observe_success"]).await;
    }
    for search in ["count@example.invalid", "rimary", "missing@example.invalid"] {
        assert_usage_search_ids(&database.pool, range, search, &[]).await;
    }

    sqlx::query("update model_requests set request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'account', coalesce(request_observation_json #> '{account}', '{}'::jsonb) || jsonb_build_object(
             'email', null)))")
        .execute(&database.pool)
        .await
        .expect("clear account email snapshots");
    assert_usage_search_ids(&database.pool, range, "primary", &["req_observe_success"]).await;
    assert_usage_search_ids(&database.pool, range, "account@", &[]).await;

    sqlx::query("update model_requests set request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'account', coalesce(request_observation_json #> '{account}', '{}'::jsonb) || jsonb_build_object(
             'name', null)))")
        .execute(&database.pool)
        .await
        .expect("clear account name snapshots");
    assert_usage_search_ids(
        &database.pool,
        range,
        "acct_observe",
        &["req_observe_success"],
    )
    .await;
    database.close().await;
}

#[tokio::test]
async fn usage_search_should_preserve_account_snapshots_after_account_changes() {
    let Some(database) = TestDatabase::create("usage_account_history").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");

    // 搜索与列表都使用请求发生时的快照，不能随当前账号资料变化或删除而改变
    for mutation in [
        "update provider_accounts set name = 'renamed', email = 'renamed@example.invalid'
         where id = 'acct_observe'",
        "delete from provider_accounts where id = 'acct_observe'",
    ] {
        sqlx::query(mutation)
            .execute(&database.pool)
            .await
            .expect("change current account");
        for search in ["account@example.invalid", "primary"] {
            assert_usage_search_ids(&database.pool, range, search, &["req_observe_success"]).await;
        }
        for search in ["renamed@example.invalid", "renamed"] {
            assert_usage_search_ids(&database.pool, range, search, &[]).await;
        }
    }
    database.close().await;
}

#[tokio::test]
async fn usage_search_should_treat_account_snapshot_wildcards_as_literals() {
    let Some(database) = TestDatabase::create("usage_account_literals").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    sqlx::query(
        "update model_requests
         set request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'account', coalesce(request_observation_json #> '{account}', '{}'::jsonb) || jsonb_build_object(
             'email', $1::text,
             'name', $2::text)))",
    )
    .bind("account_team%tag@example.invalid")
    .bind(r"primary\ops_100%")
    .execute(&database.pool)
    .await
    .expect("set account snapshots with literal wildcard characters");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");

    for search in [
        "account_team%tag@example.invalid",
        "account_",
        "account_team%",
        r"primary\",
        r"primary\ops_",
        r"primary\ops_100%",
    ] {
        assert_usage_search_ids(&database.pool, range, search, &["req_observe_success"]).await;
    }
    for search in ["account%", "account_team_", "primary_", r"primary\ops%"] {
        assert_usage_search_ids(&database.pool, range, search, &[]).await;
    }
    database.close().await;
}

async fn assert_usage_search_ids(
    pool: &PgPool,
    range: ObservabilityRange,
    search: &str,
    expected_ids: &[&str],
) {
    let page = observability_repository(pool)
        .list_usage_records(UsageRecordQuery {
            range,
            filter: UsageRecordFilter {
                search: Some(search.to_owned()),
                ..UsageRecordFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("search usage by account snapshot");

    assert_eq!(
        page.total,
        u64::try_from(expected_ids.len()).expect("expected total"),
        "search: {search}"
    );
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        expected_ids,
        "search: {search}"
    );
}

#[tokio::test]
async fn usage_and_error_search_match_key_names_instead_of_credentials() {
    let Some(database) = TestDatabase::create("usage_client_api_key_search").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let plaintext_key = format!("sk_{}", "K".repeat(43));
    sqlx::query(
        "insert into client_api_keys (id, name, key, enabled, created_at, updated_at)
         values ('key_observe', 'Production_%专用', $1, true, $2, $2)",
    )
    .bind(&plaintext_key)
    .bind(now)
    .execute(&database.pool)
    .await
    .expect("seed searchable client API key");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");

    let repository = observability_repository(&database.pool);
    for key in [plaintext_key.as_str(), "legacy-key+/123"] {
        sqlx::query("update client_api_keys set key = $1 where id = 'key_observe'")
            .bind(key)
            .execute(&database.pool)
            .await
            .unwrap();
        for (search, expected) in [
            ("production", 1),
            ("PRODUCTION_%", 1),
            ("ProductionX", 0),
            (key, 0),
            (&key[..10], 0),
        ] {
            let page = repository
                .list_usage_records(UsageRecordQuery {
                    range,
                    filter: UsageRecordFilter {
                        search: Some(search.to_owned()),
                        ..UsageRecordFilter::default()
                    },
                    current_page: 1,
                    page_size: ObservabilityPageSize::new(10).expect("page size"),
                })
                .await
                .expect("search usage by client API key name");

            assert_eq!(page.total, expected, "search: {search}");
            assert_eq!(page.items.len() as u64, expected);
            if expected == 1 {
                assert_eq!(page.items[0].id, "req_observe_success");
                assert_eq!(
                    page.items[0].client_api_key_name.as_deref(),
                    Some("Production_%专用")
                );
            }
            let errors = repository
                .list_ops_errors(OpsErrorQuery {
                    range,
                    filter: OpsErrorFilter {
                        search: Some(search.to_owned()),
                        ..Default::default()
                    },
                    current_page: 1,
                    page_size: ObservabilityPageSize::new(10).unwrap(),
                })
                .await
                .unwrap();
            assert_eq!(errors.total, expected * 2, "error search: {search}");
            assert_eq!(errors.items.len() as u64, expected * 2);
            for error in &errors.items {
                assert_eq!(
                    error.client_api_key_name.as_deref(),
                    Some("Production_%专用")
                );
            }
        }
    }
    for name in [Some("Renamed Key"), None] {
        if let Some(name) = name {
            sqlx::query("update client_api_keys set name = $1 where id = 'key_observe'")
                .bind(name)
                .execute(&database.pool)
                .await
                .unwrap();
        } else {
            sqlx::query("delete from client_api_keys where id = 'key_observe'")
                .execute(&database.pool)
                .await
                .unwrap();
        }
        let page = repository
            .list_usage_records(UsageRecordQuery {
                range,
                filter: UsageRecordFilter::default(),
                current_page: 1,
                page_size: ObservabilityPageSize::new(10).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].client_api_key_name.as_deref(), name);
        let errors = repository
            .list_ops_errors(OpsErrorQuery {
                range,
                filter: OpsErrorFilter::default(),
                current_page: 1,
                page_size: ObservabilityPageSize::new(10).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(errors.total, 2);
        assert!(
            errors
                .items
                .iter()
                .all(|error| error.client_api_key_name.as_deref() == name)
        );
    }
    database.close().await;
}

#[tokio::test]
async fn ops_search_should_treat_sql_wildcards_as_literals() {
    let Some(database) = TestDatabase::create("ops_literal_wildcard_search").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");

    let page = observability_repository(&database.pool)
        .list_ops_errors(OpsErrorQuery {
            range,
            filter: OpsErrorFilter {
                search: Some("req%".to_owned()),
                ..OpsErrorFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("ops literal wildcard search");

    assert_eq!(page.total, 0);
    database.close().await;
}

#[tokio::test]
async fn ops_errors_should_use_each_event_accounts_current_subscription() {
    let Some(database) = TestDatabase::create("ops_account_subscription").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, email, upstream_user_id, upstream_account_id,
           plan_type, authentication_kind, provider_credentials_json,
           credential_revision, credential_observed_at, has_refresh_token,
           created_at, updated_at
         ) select 'acct_retry', provider_kind, name, email, upstream_user_id,
                  'retry-workspace', 'team', authentication_kind,
                  provider_credentials_json, credential_revision, credential_observed_at,
                  has_refresh_token, created_at, updated_at
           from provider_accounts where id = 'acct_observe'",
    )
    .execute(&database.pool)
    .await
    .expect("seed same-email retry account");
    sqlx::query(
        "update ops_events set provider_account_id = 'acct_retry',
                               provider_account_ref = 'acct_retry'
         where id = 'ops_observe_retry'",
    )
    .execute(&database.pool)
    .await
    .expect("assign retry event to its own account");
    let query = admin_observability::OpsErrorQuery {
        range: admin_observability::TimeRange::new(
            now - TimeDelta::hours(1),
            now + TimeDelta::hours(1),
        )
        .expect("ops range"),
        filter: admin_observability::OpsErrorFilter::default(),
        current_page: 1,
        page_size: ObservabilityPageSize::new(10).expect("page size"),
    };
    let store = admin_observability_store(&database.pool);
    for plan in [Some("pro"), Some("plus"), None] {
        sqlx::query("update provider_accounts set plan_type = $1 where id = 'acct_observe'")
            .bind(plan)
            .execute(&database.pool)
            .await
            .expect("update current subscription");
        let page = store
            .list_ops_errors(query.clone())
            .await
            .expect("ops page");
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 2);
        let request = page
            .items
            .iter()
            .find(|error| error.source == "model_request")
            .expect("request error");
        let event = page
            .items
            .iter()
            .find(|error| error.source == "ops_event")
            .expect("retry event");
        assert_eq!(request.provider_account_email, event.provider_account_email);
        assert_eq!(request.provider_account_plan_type.as_deref(), plan);
        assert_eq!(event.provider_account_plan_type.as_deref(), Some("team"));
    }
    sqlx::query(
        "update ops_events set model_request_id = null, attempt_index = null
         where id = 'ops_observe_retry'",
    )
    .execute(&database.pool)
    .await
    .expect("detach event from request");
    let page = store
        .list_ops_errors(query.clone())
        .await
        .expect("ops page");
    assert_eq!(
        page.items
            .iter()
            .find(|error| error.source == "ops_event")
            .expect("standalone event")
            .provider_account_plan_type
            .as_deref(),
        Some("team")
    );
    sqlx::query("delete from provider_accounts where id = 'acct_retry'")
        .execute(&database.pool)
        .await
        .expect("delete retry account");
    let page = store
        .list_ops_errors(query)
        .await
        .expect("deleted account history");
    assert_eq!(page.total, 2);
    let event = page
        .items
        .iter()
        .find(|error| error.source == "ops_event")
        .expect("deleted account event");
    assert_eq!(event.provider_account_ref.as_deref(), Some("acct_retry"));
    assert_eq!(event.provider_account_plan_type, None);
    database.close().await;
}

#[tokio::test]
async fn ops_should_include_incomplete_upstream_errors() {
    let Some(database) = TestDatabase::create("ops_incomplete_error").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now).await.unwrap();
    let raw_error = r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"Invalid `previous_response_id`."}}"#;
    sqlx::query(
        "update model_requests
            set outcome = 'incomplete',
           error_kind = 'invalid_request',
           client_transport = 'websocket',
           upstream_transport = 'websocket',
           downstream_committed_at = completed_at,
           client_status_code = null,
           upstream_status_code = 400,
           error_details = $1,
           request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'error', coalesce(request_observation_json #> '{error}', '{}'::jsonb) || jsonb_build_object(
             'providerErrorCode', null)))
          where id = 'req_observe_failed'",
    )
    .bind(raw_error)
    .execute(&database.pool)
    .await
    .unwrap();
    let page = admin_observability_store(&database.pool)
        .list_ops_errors(admin_observability::OpsErrorQuery {
            range: admin_observability::TimeRange::new(
                now - TimeDelta::hours(1),
                now + TimeDelta::hours(1),
            )
            .unwrap(),
            filter: admin_observability::OpsErrorFilter {
                request_id: Some("req_observe_failed".to_owned()),
                status_code: Some(400),
                ..admin_observability::OpsErrorFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).unwrap(),
        })
        .await
        .expect("incomplete request must be available in admin error troubleshooting");
    database.close().await;

    assert_eq!(page.total, 1);
    assert_eq!(page.items.len(), 1);
    let error = &page.items[0];
    assert_eq!(error.source, "model_request");
    assert_eq!(error.failure_kind, "invalid_request");
    assert_eq!(error.upstream_status_code, Some(400));
    assert_eq!(error.client_status_code, None);
    assert_eq!(error.error_details.as_deref(), Some(raw_error));
}

#[tokio::test]
async fn ops_should_select_errors_independently_of_request_outcome() {
    let Some(database) = TestDatabase::create("ops_error_outcomes").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now).await.unwrap();
    let range =
        ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1)).unwrap();
    let repository = observability_repository(&database.pool);
    let mut results = Vec::new();
    for (outcome, error_kind, expected_count) in [
        ("incomplete", Some("upstream_unavailable"), 1),
        ("succeeded", Some("upstream_unavailable"), 1),
        ("failed", Some("upstream_unavailable"), 1),
        ("failed", None, 0),
        ("incomplete", None, 0),
        ("cancelled", Some("cancelled"), 0),
        ("incomplete", Some("cancelled"), 0),
        ("failed", Some("cancelled"), 0),
        ("succeeded", None, 0),
    ] {
        sqlx::query(
            "update model_requests set outcome = $1, error_kind = $2
              where id = 'req_observe_success'",
        )
        .bind(outcome)
        .bind(error_kind)
        .execute(&database.pool)
        .await
        .unwrap();
        let page = repository
            .list_ops_errors(OpsErrorQuery {
                range,
                filter: OpsErrorFilter {
                    request_id: Some("req_observe_success".to_owned()),
                    ..OpsErrorFilter::default()
                },
                current_page: 1,
                page_size: ObservabilityPageSize::new(10).unwrap(),
            })
            .await
            .unwrap();
        results.push((outcome, error_kind, expected_count, page));
    }
    database.close().await;

    for (outcome, error_kind, expected_count, page) in results {
        assert_eq!(
            page.total, expected_count,
            "total for outcome={outcome}, error_kind={error_kind:?}"
        );
        assert_eq!(
            page.items.len() as u64,
            expected_count,
            "items for outcome={outcome}, error_kind={error_kind:?}"
        );
        if let Some(error) = page.items.first() {
            assert_eq!(Some(error.failure_kind.as_str()), error_kind);
        }
    }
}

#[tokio::test]
async fn recovered_continuation_failure_should_be_visible_in_ops_but_hidden_from_business_metrics()
{
    let Some(database) = TestDatabase::create("observability_recovered_continuation").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    sqlx::query(
        "update model_requests
            set error_kind = 'continuation_recovery_required',
           continuation_affinity_hash = $2,
           continuation_requested = true,
           recovery_request_id = 'req_observe_success',
           recovered_at = $1,
           recovery_attempt_count = 1,
           request_observation_json = jsonb_strip_nulls(request_observation_json || jsonb_build_object(
           'continuation', coalesce(request_observation_json #> '{continuation}', '{}'::jsonb) || jsonb_build_object(
             'previousResponseIdHash', $3::text,
             'unavailableReason', 'reused_connection_lost'),
           'recovery', coalesce(request_observation_json #> '{recovery}', '{}'::jsonb) || jsonb_build_object(
             'retryDelayMs', 4000,
             'totalLatencyMs', 8000)))
          where id = 'req_observe_failed'",
    )
    .bind(now)
    .bind("a".repeat(64))
    .bind("b".repeat(64))
    .execute(&database.pool)
    .await
    .expect("mark continuation failure recovered");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");
    let repository = observability_repository(&database.pool);

    let overview = repository
        .usage_summary(range, UsageRecordFilter::default())
        .await
        .expect("usage summary without recovered intermediates");
    assert_eq!(overview.requests.request_count, 2);
    assert_eq!(overview.requests.failure_count, 0);
    assert_eq!(overview.attempts.attempt_count, 2);
    assert_eq!(overview.attempts.failure_count, 0);
    assert_eq!(overview.providers[0].request_count, 2);
    assert_eq!(overview.providers[0].failure_count, 0);

    let diagnostics = repository
        .usage_diagnostics(
            range,
            UsageRecordFilter::default(),
            DiagnosticDimension::Account,
            100,
        )
        .await
        .expect("diagnostics without recovered intermediates")
        .items;
    assert_eq!(diagnostics[0].request_count, 2);
    assert_eq!(diagnostics[0].failure_count, 0);

    let errors = repository
        .list_ops_errors(OpsErrorQuery {
            range,
            filter: OpsErrorFilter::default(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("ops errors retain recovered failures");
    assert_eq!(errors.total, 2);
    assert!(errors.items.iter().all(|error| {
        error.recovery_request_id.as_deref() == Some("req_observe_success")
            && error.recovered_at.is_some()
            && error.recovery_attempt_count == 1
    }));

    let retained: (String, Option<String>) = sqlx::query_as(
        "select outcome, recovery_request_id
           from model_requests where id = 'req_observe_failed'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load retained recovery audit row");
    assert_eq!(
        retained,
        ("failed".to_owned(), Some("req_observe_success".to_owned()))
    );
    database.close().await;
}

#[tokio::test]
async fn dashboard_account_metrics_should_partition_account_statuses() {
    let Some(database) = TestDatabase::create("dashboard_account_metrics").await else {
        return;
    };
    let now = Utc::now();
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, upstream_user_id, authentication_kind,
           provider_credentials_json, credential_revision, has_refresh_token,
           access_token_expires_at, enabled, credential_state,
           quota_access_state, quota_evidence, quota_access_observed_at,
           credential_observed_at, created_at, updated_at
         ) values
           ('acct_available', 'openai', 'available', 'user-available', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_expired', 'openai', 'expired', 'user-expired', 'oauth',
            '{}'::jsonb, 1, false, $1 - interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_rate_cooldown', 'openai', 'rate-cooldown', 'user-rate-cooldown', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_usage_limit', 'xai', 'usage-limit', 'user-usage-limit', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'exhausted', 'usage_limit_reached', $1,
            $1, $1, $1),
           ('acct_banned', 'xai', 'banned', 'user-banned', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'banned', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_disabled', 'xai', 'disabled', 'user-disabled', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', false, 'ready', 'allowed', null, $1,
            $1, $1, $1)",
    )
    .bind(now)
    .execute(&database.pool)
    .await
    .expect("seed account metric states");
    let repository = observability_repository(&database.pool);
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("dashboard range");

    let metrics = repository
        .dashboard_summary(
            admin_observability::DashboardQuery::new(admin_observability::TimeRange {
                start: range.start,
                end: range.end,
            }),
            now,
        )
        .await
        .expect("dashboard summary")
        .provider_accounts;

    assert_eq!(
        (
            metrics.total,
            metrics.normal,
            metrics.quota_exhausted,
            metrics.rate_limited,
            metrics.disabled,
            metrics.error,
        ),
        (6, 2, 1, 0, 1, 2),
    );
    assert_eq!(
        metrics.total,
        metrics.normal
            + metrics.quota_exhausted
            + metrics.rate_limited
            + metrics.disabled
            + metrics.error
    );
    database.close().await;
}

#[tokio::test]
async fn dashboard_account_metrics_with_cooldowns_should_only_reclassify_eligible_accounts() {
    let Some(database) = TestDatabase::create("dashboard_account_metrics_cooldowns").await else {
        return;
    };
    let now = Utc::now();
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, upstream_user_id, authentication_kind,
           provider_credentials_json, credential_revision, has_refresh_token,
           access_token_expires_at, enabled, credential_state,
           quota_access_state, quota_evidence, quota_access_observed_at,
           credential_observed_at, created_at, updated_at
         ) values
           ('acct_metrics_active', 'openai', 'active', 'user-active', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_metrics_active_cooling', 'openai', 'active-cooling', 'user-active-cooling', 'oauth',
            '{}'::jsonb, 2, false, $1 + interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_metrics_ready_quota', 'openai', 'ready-quota', 'user-ready-quota', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'exhausted', 'provider_denied', $1,
            $1, $1, $1),
           ('acct_metrics_cooling', 'openai', 'cooling', 'user-cooling', 'oauth',
            '{}'::jsonb, 3, false, $1 + interval '1 day', true, 'ready', 'exhausted', 'provider_denied', $1,
            $1, $1, $1),
           ('acct_metrics_stale_cooldown', 'openai', 'stale-cooldown', 'user-stale-cooldown', 'oauth',
            '{}'::jsonb, 4, false, $1 + interval '1 day', true, 'ready', 'allowed', null, $1,
            $1, $1, $1),
           ('acct_metrics_persistent_quota', 'openai', 'persistent-quota', 'user-persistent-quota', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', true, 'ready', 'exhausted', 'provider_denied', $1,
            $1, $1, $1),
           ('acct_metrics_expired', 'openai', 'expired', 'user-expired', 'oauth',
            '{}'::jsonb, 5, false, $1 - interval '1 second', true, 'ready', 'exhausted', 'provider_denied', $1,
            $1, $1, $1),
           ('acct_metrics_unknown', 'openai', 'unknown', 'user-unknown', 'oauth',
            '{}'::jsonb, 6, false, $1 + interval '1 day', true, 'unknown', 'unknown', null, null,
            $1, $1, $1),
           ('acct_metrics_disabled', 'openai', 'disabled', 'user-disabled', 'oauth',
            '{}'::jsonb, 1, false, $1 + interval '1 day', false, 'ready', 'allowed', null, $1,
            $1, $1, $1)",
    )
    .bind(now)
    .execute(&database.pool)
    .await
    .expect("seed cooldown account metric states");
    let cooldowns = StaticCooldowns::new([
        test_cooldown("acct_metrics_active_cooling", 2),
        test_cooldown("acct_metrics_cooling", 3),
        test_cooldown("acct_metrics_expired", 5),
        test_cooldown("acct_metrics_unknown", 6),
        test_cooldown("acct_metrics_stale_cooldown", 3),
    ]);
    let repository = PgObservabilityRepository::new(
        database.pool.clone(),
        Some(Arc::new(cooldowns)),
        observability_query_budget(),
        None,
    );
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("dashboard range");

    let metrics = repository
        .dashboard_summary(
            admin_observability::DashboardQuery::new(admin_observability::TimeRange {
                start: range.start,
                end: range.end,
            }),
            now,
        )
        .await
        .expect("dashboard summary")
        .provider_accounts;

    assert_eq!(
        (
            metrics.total,
            metrics.normal,
            metrics.rate_limited,
            metrics.quota_exhausted,
            metrics.disabled,
            metrics.error,
        ),
        (9, 1, 2, 3, 1, 2),
    );
    assert_eq!(
        metrics.total,
        metrics.normal
            + metrics.rate_limited
            + metrics.quota_exhausted
            + metrics.disabled
            + metrics.error
    );
    database.close().await;
}

struct StaticCooldowns {
    reads: std::sync::atomic::AtomicUsize,
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    delay: Duration,
    cooldowns: BTreeMap<ProviderAccountId, ProviderCooldown>,
}

impl StaticCooldowns {
    fn new(cooldowns: impl IntoIterator<Item = ProviderCooldown>) -> Self {
        Self {
            reads: Default::default(),
            active: Default::default(),
            peak: Default::default(),
            delay: Duration::ZERO,
            cooldowns: cooldowns
                .into_iter()
                .map(|cooldown| (cooldown.account_id().clone(), cooldown))
                .collect(),
        }
    }
}

impl ProviderCooldownPort for StaticCooldowns {
    fn put_if_later(
        &self,
        _cooldown: ProviderCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async move {
            use std::sync::atomic::Ordering::Relaxed;
            self.reads.fetch_add(1, Relaxed);
            let active = self.active.fetch_add(1, Relaxed) + 1;
            self.peak.fetch_max(active, Relaxed);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.active.fetch_sub(1, Relaxed);
            Ok(self.cooldowns.get(account_id).cloned())
        })
    }

    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn put_scoped_if_later(
        &self,
        _cooldown: ProviderScopedCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn clear_all<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn record_capacity_failure<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _window: Duration,
        _in_flight: u32,
    ) -> BoxFuture<'a, Result<u32, ProviderStoreError>> {
        Box::pin(async { Ok(0) })
    }

    fn clear_after_success<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: gateway_core::account::CredentialRevision,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn capacity_peak_in_flight<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<u32>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }
}

fn test_cooldown(account_id: &str, revision: u64) -> ProviderCooldown {
    ProviderCooldown::new(
        ProviderAccountId::new(account_id).expect("test account ID"),
        CredentialRevision::new(revision).expect("test revision"),
        SystemTime::now() + Duration::from_secs(60),
    )
}

#[tokio::test]
async fn calculated_usage_billing_facts_keep_only_completed_calculated_costs() {
    let Some(database) = TestDatabase::create("calculated_usage_billing_facts").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    seed_calculated_billing_facts(&database.pool, now)
        .await
        .expect("seed calculated billing facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");
    let repository = observability_repository(&database.pool);

    let facts = repository
        .usage_calculated_billing_facts(
            range,
            UsageRecordFilter::default(),
            admin_observability::Granularity::FifteenMinutes,
        )
        .try_collect::<Vec<_>>()
        .await
        .expect("calculated usage billing facts");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].provider_kind, "openai");
    assert_eq!(facts[0].upstream_model_id, "gpt-5.5");
    assert_eq!(facts[0].input_tokens, Some(800));
    assert_eq!(facts[0].output_tokens, Some(200));
    assert_eq!(facts[0].service_tier.as_deref(), Some("priority"));
    assert_eq!(facts[0].total.amount.as_str(), "1.25");

    let store = admin_observability_store(&database.pool);
    let facts = store
        .usage_calculated_billing_facts(
            admin_observability::TimeRange::new(range.start, range.end)
                .expect("admin observability range"),
            admin_observability::UsageFilter::default(),
            admin_observability::Granularity::FifteenMinutes,
        )
        .try_collect::<Vec<_>>()
        .await
        .expect("admin calculated usage billing facts");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].service_tier.as_deref(), Some("priority"));
    assert_eq!(facts[0].total.amount.as_str(), "1.25");

    database.close().await;
}

#[tokio::test]
async fn admin_observability_adapter_preserves_utc_queries_metrics_costs_and_details() {
    let Some(database) = TestDatabase::create("admin_observability").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range =
        admin_observability::TimeRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
            .expect("admin observability range");
    let store = admin_observability_store(&database.pool);

    let dashboard = store
        .dashboard_summary(
            admin_observability::DashboardQuery::new(admin_observability::TimeRange {
                start: range.start,
                end: range.end,
            }),
            now,
        )
        .await
        .expect("admin dashboard summary");
    assert_eq!(dashboard.range, range);
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        3
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.first_token_latency_sum_ms)
            .sum::<u64>(),
        120
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.latency_sum_ms)
            .sum::<u64>(),
        900
    );
    assert_eq!(dashboard.provider_accounts.total, 1);
    assert_eq!(dashboard.account_usage[0].request_count, 1);
    assert_eq!(dashboard.account_usage[0].request_buckets.len(), 24);
    assert_eq!(
        dashboard.account_usage[0]
            .request_buckets
            .iter()
            .map(|bucket| bucket.request_count)
            .sum::<u64>(),
        1,
    );
    assert_eq!(dashboard.recent_requests.len(), 1);
    assert_eq!(
        dashboard.recent_requests[0].service_tier.as_deref(),
        Some("priority")
    );
    assert_eq!(dashboard.recent_requests[0].id, "req_observe_success");
    assert_eq!(
        dashboard.recent_requests[0]
            .cost_amount
            .as_ref()
            .expect("dashboard request cost")
            .as_str(),
        "1.25",
    );

    let dashboard_trend = store
        .dashboard_trend(range, admin_observability::Granularity::FifteenMinutes)
        .await
        .expect("admin dashboard trend");
    assert_eq!(
        dashboard_trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        3,
    );
    assert_eq!(
        dashboard_trend
            .iter()
            .map(|point| point.metrics.failure_count)
            .sum::<u64>(),
        1,
    );
    assert!(
        dashboard
            .trend
            .iter()
            .chain(&dashboard_trend)
            .all(|point| point.costs.is_empty()),
        "Dashboard 趋势不应触发未展示的成本聚合",
    );

    let trend = store
        .usage_trend(
            range,
            admin_observability::UsageFilter {
                outcome: Some(admin_observability::RequestOutcome::Succeeded),
                ..admin_observability::UsageFilter::default()
            },
            admin_observability::Granularity::FifteenMinutes,
        )
        .await
        .expect("admin usage trend");
    assert_eq!(
        trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        2,
    );
    assert_eq!(
        trend
            .iter()
            .flat_map(|point| &point.costs)
            .next()
            .expect("trend cost")
            .amount
            .as_str(),
        "1.25",
    );

    let first_page = store
        .list_usage_records(admin_observability::UsageQuery {
            range,
            filter: admin_observability::UsageFilter::default(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(1).expect("page size"),
        })
        .await
        .expect("first usage page");
    assert_eq!(first_page.total, 1);
    assert_eq!(first_page.items.len(), 1);
    assert_eq!(first_page.current_page, 1);
    assert_eq!(first_page.page_size, 1);
    assert_eq!(
        first_page.items[0].service_tier.as_deref(),
        Some("priority")
    );

    let deep_page = store
        .list_usage_records(admin_observability::UsageQuery {
            range,
            filter: admin_observability::UsageFilter::default(),
            current_page: 129,
            page_size: ObservabilityPageSize::new(1).expect("page size"),
        })
        .await
        .expect("direct deep usage page");
    assert_eq!(deep_page.current_page, 129);
    assert_eq!(deep_page.page_size, 1);
    assert_eq!(deep_page.total, 1);
    assert!(deep_page.items.is_empty());

    let filtered = store
        .list_usage_records(admin_observability::UsageQuery {
            range,
            filter: admin_observability::UsageFilter {
                client_api_key_ref: Some("key_observe".to_owned()),
                request_id: Some("req_observe_success".to_owned()),
                provider_account_ref: Some("acct_observe".to_owned()),
                operation: Some("responses".to_owned()),
                provider_kind: Some("openai".to_owned()),
                model: Some("upstream-model".to_owned()),
                outcome: Some(admin_observability::RequestOutcome::Succeeded),
                status_code: Some(200),
                transport: Some("http_sse".to_owned()),
                attempt_index: Some(1),
                response_id: Some("resp_observe_success".to_owned()),
                upstream_request_id: Some("upstream_req_success".to_owned()),
                search: Some("req_observe_success".to_owned()),
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("fully filtered usage page");
    assert_eq!(filtered.total, 1);
    assert_eq!(filtered.items[0].id, "req_observe_success");
    assert_eq!(filtered.items[0].service_tier.as_deref(), Some("priority"));

    let other_outcome = store
        .list_usage_records(admin_observability::UsageQuery {
            range,
            filter: admin_observability::UsageFilter {
                outcome: Some(
                    admin_observability::RequestOutcome::new("provider_future_state")
                        .expect("bounded other outcome"),
                ),
                ..admin_observability::UsageFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("other outcome filter should reach PostgreSQL");
    assert_eq!(other_outcome.total, 0);
    assert!(other_outcome.items.is_empty());

    let detail = store
        .usage_record_detail("req_observe_success")
        .await
        .expect("admin usage detail");
    assert_eq!(
        detail.request.outcome,
        admin_observability::RequestOutcome::Succeeded
    );
    assert_eq!(detail.request.service_tier.as_deref(), Some("priority"));
    assert_eq!(detail.request.routing_scope, "groups");
    assert_eq!(detail.request.routing_group_refs, ["grp_history"]);
    assert_eq!(
        detail.request.routing_group_names_snapshot,
        ["Historical group"]
    );
    assert_eq!(
        detail.request.upstream_request_id.as_deref(),
        Some("upstream_req_success")
    );
    assert_eq!(detail.attempts.len(), 1);
    assert_eq!(
        detail.attempts[0].outcome,
        admin_observability::RequestOutcome::Succeeded
    );
    assert!(
        store
            .usage_record_detail("req_observe_failed")
            .await
            .is_ok()
    );
    assert!(
        store
            .usage_record_detail("req_observe_uncommitted")
            .await
            .is_ok()
    );

    let overview = store
        .usage_summary(range, admin_observability::UsageFilter::default())
        .await
        .expect("admin usage overview");
    assert_eq!(overview.range, range);
    assert_eq!(overview.providers[0].provider_kind, "openai");
    assert_eq!(overview.attempts.costs[0].amount.as_str(), "1.25");

    let diagnostics = store
        .usage_diagnostics(
            range,
            admin_observability::UsageFilter::default(),
            admin_observability::DiagnosticDimension::Account,
            100,
        )
        .await
        .expect("admin diagnostics")
        .items;
    assert_eq!(diagnostics[0].key, "acct_observe");
    assert_eq!(diagnostics[0].name, "account@example.invalid");
    assert_eq!(diagnostics[0].cost_coverage.provider_reported_count, 1);
    assert_eq!(diagnostics[0].costs[0].amount.as_str(), "1.25");

    let errors = store
        .list_ops_errors(admin_observability::OpsErrorQuery {
            range,
            filter: admin_observability::OpsErrorFilter {
                request_id: Some("req_observe_failed".to_owned()),
                provider_kind: Some("openai".to_owned()),
                provider_account_ref: Some("acct_observe".to_owned()),
                operation: Some("responses".to_owned()),
                model: Some("upstream-model".to_owned()),
                status_code: Some(429),
                search: Some("req_observe_failed".to_owned()),
                ..admin_observability::OpsErrorFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("admin ops errors");
    assert_eq!(errors.total, 2);
    assert!(errors.items.iter().all(|item| item.occurred_at <= now));

    let deep_errors = store
        .list_ops_errors(admin_observability::OpsErrorQuery {
            range,
            filter: admin_observability::OpsErrorFilter::default(),
            current_page: 129,
            page_size: ObservabilityPageSize::new(1).expect("page size"),
        })
        .await
        .expect("direct deep ops page");
    assert_eq!(deep_errors.current_page, 129);
    assert_eq!(deep_errors.page_size, 1);
    assert_eq!(deep_errors.total, 2);
    assert!(deep_errors.items.is_empty());

    for filter in [
        admin_observability::OpsErrorFilter {
            client_api_key_ref: Some("missing-key".to_owned()),
            ..admin_observability::OpsErrorFilter::default()
        },
        admin_observability::OpsErrorFilter {
            operation: Some("missing-operation".to_owned()),
            ..admin_observability::OpsErrorFilter::default()
        },
        admin_observability::OpsErrorFilter {
            transport: Some("missing-transport".to_owned()),
            ..admin_observability::OpsErrorFilter::default()
        },
        admin_observability::OpsErrorFilter {
            attempt_index: Some(99),
            ..admin_observability::OpsErrorFilter::default()
        },
        admin_observability::OpsErrorFilter {
            response_id: Some("missing-response".to_owned()),
            ..admin_observability::OpsErrorFilter::default()
        },
        admin_observability::OpsErrorFilter {
            upstream_request_id: Some("missing-upstream-request".to_owned()),
            ..admin_observability::OpsErrorFilter::default()
        },
    ] {
        let page = store
            .list_ops_errors(admin_observability::OpsErrorQuery {
                range,
                filter,
                current_page: 1,
                page_size: ObservabilityPageSize::new(10).expect("page size"),
            })
            .await
            .expect("fully forwarded ops filter");
        assert_eq!(page.total, 0);
    }

    database.close().await;
}

#[tokio::test]
async fn dashboard_summary_totals_include_history_outside_selected_range() {
    let Some(database) = TestDatabase::create("observability_dashboard_totals").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    sqlx::query(
        "update model_requests
         set started_at = $1 - interval '2 hours'
         where id = 'req_observe_success'",
    )
    .bind(now)
    .execute(&database.pool)
    .await
    .expect("move historical request outside selected range");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");
    let repository = observability_repository(&database.pool);

    let dashboard = repository
        .dashboard_summary(
            admin_observability::DashboardQuery::new(admin_observability::TimeRange {
                start: range.start,
                end: range.end,
            }),
            now,
        )
        .await
        .expect("dashboard summary");
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        2
    );
    assert!(dashboard.account_usage.is_empty());

    let explicit_account_usage = repository
        .provider_account_usage(
            ProviderAccountUsageQuery::for_accounts(range, vec!["acct_observe".to_owned()])
                .expect("account usage query"),
        )
        .await
        .expect("provider account usage");
    assert_eq!(explicit_account_usage.len(), 1);
    assert_eq!(explicit_account_usage[0].request_count, 0);

    assert_eq!(
        (
            dashboard.totals.request_count,
            dashboard.totals.input_tokens,
            dashboard.totals.cached_tokens,
            dashboard.totals.total_tokens,
        ),
        (3, 100, 40, 120),
    );
    assert_eq!(
        dashboard
            .totals
            .billing_usd
            .as_ref()
            .map(|amount| amount.as_str()),
        Some("1.25"),
    );

    database.close().await;
}

#[tokio::test]
async fn observability_queries_preserve_request_account_cost_and_diagnostic_facts() {
    let Some(database) = TestDatabase::create("observability").await else {
        return;
    };
    let now = Utc::now();
    seed_observability_facts(&database.pool, now)
        .await
        .expect("seed observability facts");
    let range = ObservabilityRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1))
        .expect("observability range");
    let repository = observability_repository(&database.pool);

    let dashboard = repository
        .dashboard_summary(
            admin_observability::DashboardQuery::new(admin_observability::TimeRange {
                start: range.start,
                end: range.end,
            }),
            now,
        )
        .await
        .expect("dashboard summary");
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        3
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.success_count)
            .sum::<u64>(),
        2
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.failure_count)
            .sum::<u64>(),
        1
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.caller_error_count)
            .sum::<u64>(),
        0
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.cache_eligible_request_count)
            .sum::<u64>(),
        1
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.cache_hit_request_count)
            .sum::<u64>(),
        1
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .find_map(|point| point.metrics.latency_percentiles.p50_ms)
            .expect("latency p50")
            .as_f64(),
        900.0
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .find_map(|point| point.metrics.latency_percentiles.p95_ms)
            .expect("latency p95")
            .as_f64(),
        900.0
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .find_map(|point| point.metrics.latency_percentiles.p99_ms)
            .expect("latency p99")
            .as_f64(),
        900.0
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .find_map(|point| point.metrics.first_token_latency_percentiles.p50_ms)
            .expect("first token p50")
            .as_f64(),
        120.0
    );
    assert_eq!(dashboard.provider_accounts.total, 1);
    assert_eq!(dashboard.account_usage[0].request_count, 1);
    assert_eq!(dashboard.account_usage[0].request_buckets.len(), 24);
    assert_eq!(
        dashboard.account_usage[0]
            .request_buckets
            .iter()
            .map(|bucket| bucket.request_count)
            .sum::<u64>(),
        1,
    );
    assert_eq!(dashboard.recent_requests.len(), 1);
    assert_eq!(dashboard.recent_requests[0].id, "req_observe_success");
    assert_eq!(
        dashboard.recent_requests[0].service_tier.as_deref(),
        Some("priority")
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        3
    );
    assert_eq!(
        dashboard
            .trend
            .iter()
            .map(|point| point.metrics.failure_count)
            .sum::<u64>(),
        1
    );

    let account_usage = repository
        .provider_account_usage(
            ProviderAccountUsageQuery::for_accounts(range, vec!["acct_observe".to_owned()])
                .expect("account usage query")
                .with_hourly_request_buckets()
                .expect("account request timeline"),
        )
        .await
        .expect("provider account usage");
    assert_eq!(account_usage[0].request_count, 1);
    assert_eq!(account_usage[0].authentication_kind, "oauth");
    assert_eq!(account_usage[0].models[0].request_count, 1);
    assert_eq!(account_usage[0].cost_coverage.provider_reported_count, 1);
    assert_eq!(account_usage[0].cost_coverage.unavailable_count, 0);
    assert_eq!(account_usage[0].costs[0].amount.as_str(), "1.25");
    assert_eq!(
        account_usage[0]
            .request_buckets
            .iter()
            .map(|bucket| bucket.request_count)
            .sum::<u64>(),
        1,
    );
    assert_eq!(
        (
            account_usage[0].image_input_tokens,
            account_usage[0].image_output_tokens,
            account_usage[0].image_request_count,
            account_usage[0].image_request_failed_count,
            account_usage[0].models[0].image_request_count,
            account_usage[0].models[0].image_request_failed_count,
        ),
        (Some(31), Some(9), 1, 0, 1, 0)
    );

    let usage_page = repository
        .list_usage_records(UsageRecordQuery {
            range,
            filter: UsageRecordFilter {
                provider_account_ref: Some("acct_observe".to_owned()),
                ..UsageRecordFilter::default()
            },
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("usage records");
    assert_eq!(usage_page.total, 1);
    let successful_image = usage_page
        .items
        .iter()
        .find(|record| record.id == "req_observe_success")
        .expect("successful image usage record");
    assert_eq!(
        (
            successful_image.service_tier.as_deref(),
            successful_image
                .provider_account_authentication_kind
                .as_deref(),
            successful_image.image_input_tokens,
            successful_image.image_output_tokens,
        ),
        (Some("priority"), Some("oauth"), Some(31), Some(9))
    );

    let successful_detail = repository
        .usage_record_detail("req_observe_success")
        .await
        .expect("successful usage detail");
    assert_eq!(
        (
            successful_detail.request.websocket_pool.as_deref(),
            successful_detail.request.image_generation_requested,
            successful_detail.request.image_generation_succeeded,
        ),
        (Some("reuse"), true, Some(true))
    );

    assert!(
        repository
            .usage_record_detail("req_observe_failed")
            .await
            .is_ok()
    );

    let overview = repository
        .usage_summary(range, UsageRecordFilter::default())
        .await
        .expect("usage summary");
    assert_eq!(overview.attempts.attempt_count, 4);
    assert_eq!(overview.attempts.failure_count, 2);
    assert_eq!(overview.requests.request_count, 3);
    assert_eq!(overview.requests.failure_count, 1);
    assert_eq!(
        (
            overview.providers[0].request_count,
            overview.providers[0].attempt_count,
            overview.providers[0].failure_count,
            overview.providers[0].total_tokens,
        ),
        (3, 4, 1, 120),
    );

    let succeeded = repository
        .usage_summary(
            range,
            UsageRecordFilter {
                outcome: Some("succeeded".to_owned()),
                ..UsageRecordFilter::default()
            },
        )
        .await
        .expect("filtered usage summary");
    assert_eq!(succeeded.requests.cache_eligible_request_count, 1);
    assert_eq!(succeeded.requests.cache_hit_request_count, 1);
    assert_eq!(
        succeeded.requests.cache_hit_request_count,
        succeeded.requests.cache_eligible_request_count
    );
    assert_eq!(
        succeeded
            .requests
            .latency_percentiles
            .p50_ms
            .expect("filtered p50")
            .as_f64(),
        900.0
    );

    let diagnostics = repository
        .usage_diagnostics(
            range,
            UsageRecordFilter::default(),
            DiagnosticDimension::Account,
            100,
        )
        .await
        .expect("usage diagnostics")
        .items;
    assert_eq!(diagnostics[0].key, "acct_observe");
    assert_eq!(diagnostics[0].name, "account@example.invalid");
    assert_eq!(diagnostics[0].request_count, 3);
    assert_eq!(diagnostics[0].success_count, 2);
    assert_eq!(diagnostics[0].failure_count, 1);
    assert_eq!(diagnostics[0].retry_count, 1);
    assert_eq!(diagnostics[0].costs[0].amount.as_str(), "1.25");

    let errors = repository
        .list_ops_errors(OpsErrorQuery {
            range,
            filter: OpsErrorFilter::default(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).expect("page size"),
        })
        .await
        .expect("ops errors");
    assert_eq!(errors.total, 2);
    let request_error = errors
        .items
        .iter()
        .find(|error| error.source == "model_request")
        .expect("request error");
    assert_eq!(request_error.client_status_code, Some(502));
    assert_eq!(request_error.upstream_status_code, Some(429));
    assert_eq!(request_error.client_ip.as_deref(), Some("203.0.113.9"));
    assert_eq!(
        request_error.user_agent.as_deref(),
        Some("codex-cli/0.144.0")
    );
    assert_eq!(
        request_error.requested_model_id.as_deref(),
        Some("public-model")
    );
    let attempt_error = errors
        .items
        .iter()
        .find(|error| error.source == "ops_event")
        .expect("attempt error");
    assert_eq!(attempt_error.client_status_code, None);
    assert_eq!(attempt_error.upstream_status_code, Some(429));
    assert_eq!(attempt_error.endpoint.as_deref(), Some("/v1/responses"));
    assert_eq!(attempt_error.client_ip.as_deref(), Some("203.0.113.9"));
    assert_eq!(
        attempt_error.user_agent.as_deref(),
        Some("codex-cli/0.144.0")
    );

    database.close().await;
}

async fn seed_observability_facts(
    pool: &PgPool,
    now: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, email, upstream_user_id,
           upstream_account_id, plan_type, authentication_kind,
           provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, enabled, credential_state,
           credential_observed_at, created_at, updated_at
         ) values (
           'acct_observe', 'openai', 'primary', 'account@example.invalid',
           'user-observe', null, 'pro', 'oauth', '{}'::jsonb, 1, false, $1 + interval '1 day',
           true, 'ready', $1, $1, $1
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, operation, client_transport, requested_model_id, provider_kind, provider_account_id, provider_account_ref, upstream_model_id, upstream_transport, attempt_count, upstream_send_state, outcome, client_status_code, input_tokens, output_tokens, total_tokens, cost_source, started_at, deadline_at, completed_at, request_observation_json
         ) values (
           'req_observe_uncommitted', 'key_observe', 'responses', 'http_sse', 'public-model', 'openai', 'acct_observe', 'acct_observe', 'upstream-model', 'http_sse', 1, 'sent', 'succeeded', 200, 900, 900, 1800, 'unavailable', $1 - interval '15 minutes', $1 + interval '10 minutes', $1 - interval '14 minutes',
           jsonb_strip_nulls(jsonb_build_object(
           'request', jsonb_build_object(
             'configRevision', 1,
             'protocol', 'openai',
             'endpoint', '/v1/responses',
             'compact', false),
           'account', jsonb_build_object(
             'name', 'primary',
             'email', 'account@example.invalid',
             'authenticationKind', 'oauth'),
           'timings', jsonb_build_object(
             'local', jsonb_build_object(
               'latencyMs', 650)),
           'routing', jsonb_build_object(
             'scope', 'all',
             'groupRefs', '{}'::text[],
             'groupNamesSnapshot', '[]'::jsonb)))
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, operation, client_transport, requested_model_id, provider_kind, provider_account_id, provider_account_ref, upstream_model_id, upstream_transport, service_tier, attempt_count, upstream_send_state, downstream_committed_at, outcome, client_status_code, upstream_status_code, client_response_id, upstream_request_id, upstream_response_id, input_tokens, output_tokens, cached_tokens, cache_write_tokens, reasoning_tokens, image_input_tokens, image_output_tokens, total_tokens, image_generation_requested, image_generation_succeeded, cost_source, cost_amount, cost_currency, started_at, deadline_at, completed_at, request_observation_json
         ) values (
           'req_observe_success', 'key_observe', 'responses', 'http_sse', 'public-model', 'openai', 'acct_observe', 'acct_observe', 'upstream-model', 'http_sse', 'priority', 1, 'sent', $1 - interval '19 minutes', 'succeeded', 200, 200, 'resp_observe_success', 'upstream_req_success', 'upstream_resp_success', 100, 20, 40, 3, 5, 31, 9, 120, true, true, 'provider_reported', 1.25, 'USD', $1 - interval '20 minutes', $1 + interval '10 minutes', $1 - interval '19 minutes',
           jsonb_strip_nulls(jsonb_build_object(
           'request', jsonb_build_object(
             'configRevision', 1,
             'protocol', 'openai',
             'endpoint', '/v1/responses',
             'compact', false),
           'transport', jsonb_build_object(
             'websocketPool', 'reuse'),
           'account', jsonb_build_object(
             'name', 'primary',
             'email', 'account@example.invalid',
             'authenticationKind', 'oauth'),
           'timings', jsonb_build_object(
             'local', jsonb_build_object(
               'firstTokenMs', 120,
               'latencyMs', 900)),
           'routing', jsonb_build_object(
             'scope', 'groups',
             'groupRefs', array['grp_history'],
             'groupNamesSnapshot', jsonb_build_array('Historical group'))))
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, operation, client_transport, requested_model_id, service_tier, provider_kind, provider_account_id, provider_account_ref, upstream_model_id, upstream_transport, attempt_count, upstream_send_state, outcome, client_status_code, upstream_status_code, error_kind, input_tokens, cached_tokens, image_generation_requested, image_generation_succeeded, cost_source, request_kind, started_at, deadline_at, completed_at, request_observation_json
         ) values (
           'req_observe_failed', 'key_observe', 'responses', 'http_sse', 'public-model', 'priority', 'openai', 'acct_observe', 'acct_observe', 'upstream-model', 'http_sse', 2, 'sent', 'failed', 502, 429, 'rate_limited', 0, 0, true, false, 'unavailable', 'root', $1 - interval '10 minutes', $1 + interval '20 minutes', $1 - interval '9 minutes',
           jsonb_strip_nulls(jsonb_build_object(
           'request', jsonb_build_object(
             'configRevision', 1,
             'protocol', 'openai',
             'endpoint', '/v1/responses',
             'clientIp', '203.0.113.9',
             'userAgent', 'codex-cli/0.144.0',
             'reasoningEffort', 'medium',
             'reasoningPreset', null,
             'subagentKind', null,
             'compact', false),
           'account', jsonb_build_object(
             'name', 'primary',
             'email', 'account@example.invalid',
             'authenticationKind', 'oauth'),
           'error', jsonb_build_object(
             'providerErrorCode', 'rate_limit',
             'message', 'upstream limited',
             'retryAfterMs', 1000),
           'timings', jsonb_build_object(
             'local', jsonb_build_object(
               'latencyMs', 700)),
           'routing', jsonb_build_object(
             'scope', 'all',
             'groupRefs', '{}'::text[],
             'groupNamesSnapshot', '[]'::jsonb)))
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into ops_events (
           id, model_request_id, attempt_index, level, component, operation,
           provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, failure_kind, status_code,
           provider_account_name_snapshot, provider_account_email_snapshot,
           provider_account_authentication_kind_snapshot,
           provider_error_code, retry_after_ms, latency_ms, message, created_at
         ) values (
           'ops_observe_retry', 'req_observe_failed', 1, 'warning', 'routing', 'responses',
           'openai', 'acct_observe', 'acct_observe',
           'upstream-model', 'rate_limited', 429,
           'primary', 'account@example.invalid', 'oauth',
           'rate_limit', 1000, 300,
           'first account was limited', $1 - interval '9 minutes 30 seconds'
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

async fn seed_calculated_billing_facts(
    pool: &PgPool,
    now: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, operation, client_transport, requested_model_id, provider_kind, provider_account_id, provider_account_ref, upstream_model_id, upstream_transport, attempt_count, upstream_send_state, downstream_committed_at, outcome, client_status_code, input_tokens, output_tokens, cached_tokens, cache_write_tokens, total_tokens, service_tier, cost_source, cost_amount, cost_currency, started_at, deadline_at, completed_at, request_observation_json
         ) values (
           'req_observe_calculated', 'key_observe', 'responses', 'http_sse', 'public-model', 'openai', 'acct_observe', 'acct_observe', 'gpt-5.5', 'http_sse', 1, 'sent', $1 - interval '29 minutes', 'succeeded', 200, 800, 200, 0, 0, 1000, 'priority', 'calculated', 1.25, 'USD', $1 - interval '30 minutes', $1 + interval '10 minutes', $1 - interval '29 minutes',
           jsonb_strip_nulls(jsonb_build_object(
           'request', jsonb_build_object(
             'configRevision', 1,
             'protocol', 'openai',
             'endpoint', '/v1/responses',
             'compact', false),
           'account', jsonb_build_object(
             'name', 'primary',
             'email', 'account@example.invalid',
             'authenticationKind', 'oauth'),
           'routing', jsonb_build_object(
             'scope', 'all',
             'groupRefs', '{}'::text[],
             'groupNamesSnapshot', '[]'::jsonb)))
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, operation, client_transport, requested_model_id, provider_kind, provider_account_id, provider_account_ref, upstream_model_id, upstream_transport, attempt_count, upstream_send_state, outcome, client_status_code, input_tokens, output_tokens, total_tokens, cost_source, cost_amount, cost_currency, started_at, deadline_at, completed_at, request_observation_json
         ) values (
           'req_observe_calculated_uncommitted', 'key_observe', 'responses', 'http_sse', 'public-model', 'openai', 'acct_observe', 'acct_observe', 'gpt-5.5', 'http_sse', 1, 'sent', 'succeeded', 200, 800, 200, 1000, 'calculated', 1.25, 'USD', $1 - interval '40 minutes', $1 + interval '10 minutes', $1 - interval '39 minutes',
           jsonb_strip_nulls(jsonb_build_object(
           'request', jsonb_build_object(
             'configRevision', 1,
             'protocol', 'openai',
             'endpoint', '/v1/responses',
             'compact', false),
           'account', jsonb_build_object(
             'name', 'primary',
             'email', 'account@example.invalid',
             'authenticationKind', 'oauth'),
           'routing', jsonb_build_object(
             'scope', 'all',
             'groupRefs', '{}'::text[],
             'groupNamesSnapshot', '[]'::jsonb)))
         )",
    )
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn calendar_trends_share_exact_day_boundaries_for_requests_costs_and_empty_buckets() {
    use gateway_core::time::DeploymentTimeZone;
    for (name, at) in [
        ("America/New_York", "2026-11-01T05:30:00Z"),
        ("America/Havana", "2026-11-01T04:30:00Z"),
        ("Asia/Kathmandu", "2026-10-01T00:00:00Z"),
    ] {
        let Some(database) = TestDatabase::create("calendar_trends").await else {
            return;
        };
        let timezone: DeploymentTimeZone = name.parse().unwrap();
        let at: chrono::DateTime<Utc> = at.parse().unwrap();
        seed_observability_facts(&database.pool, at).await.unwrap();
        seed_calculated_billing_facts(&database.pool, at)
            .await
            .unwrap();
        sqlx::query(
            "update model_requests set started_at = $1, completed_at = $1 + interval '1 second'",
        )
        .bind(at)
        .execute(&database.pool)
        .await
        .unwrap();
        let range =
            admin_observability::TimeRange::new(at - TimeDelta::days(32), at + TimeDelta::days(1))
                .unwrap();
        let store = admin_observability_store(&database.pool).with_timezone(timezone);
        let points = store
            .usage_trend(
                range,
                Default::default(),
                admin_observability::Granularity::Day,
            )
            .await
            .expect("calendar trend");
        let populated = points.iter().find(|p| p.metrics.request_count > 0).unwrap();
        assert_eq!(
            populated.bucket_start,
            timezone.day_start(at).unwrap(),
            "{name}"
        );
        assert_eq!(populated.metrics.request_count, 5);
        assert_eq!(populated.costs[0].amount.as_str(), "2.5");
        let mut boundary = timezone.day_start(range.start).unwrap();
        for point in &points {
            assert_eq!(point.bucket_start, boundary, "{name}");
            boundary = timezone.days_after(boundary, 1).unwrap();
        }
        let facts = store
            .usage_calculated_billing_facts(
                range,
                Default::default(),
                admin_observability::Granularity::Day,
            )
            .try_collect::<Vec<_>>()
            .await
            .expect("calendar cost buckets");
        // 只有已完整交付的 calculated 费用参与趋势事实；其他费用仍按相同边界聚合
        assert_eq!(facts.len(), 1);
        assert!(
            facts
                .iter()
                .all(|fact| fact.bucket_start == populated.bucket_start)
        );
        database.close().await;
    }
}

#[tokio::test]
async fn account_hourly_buckets_use_utc_hours_independently_of_calendar_metrics() {
    use admin_observability::CalendarPeriod;
    use gateway_core::time::DeploymentTimeZone;
    for (name, end, early, metrics_count, hourly_count) in [
        (
            "America/New_York",
            "2026-11-02T04:59:00Z",
            "2026-11-01T04:10:00Z",
            2,
            1,
        ),
        (
            "America/New_York",
            "2026-03-09T03:59:00Z",
            "2026-03-08T04:10:00Z",
            1,
            2,
        ),
        (
            "Asia/Kathmandu",
            "2026-10-01T04:20:00Z",
            "2026-09-30T18:05:00Z",
            1,
            2,
        ),
        ("UTC", "2026-10-02T00:00:00Z", "2026-10-01T23:10:00Z", 0, 2),
    ] {
        let Some(database) = TestDatabase::create("account_hourly_calendar").await else {
            return;
        };
        let timezone: DeploymentTimeZone = name.parse().unwrap();
        let end: chrono::DateTime<Utc> = end.parse().unwrap();
        let early: chrono::DateTime<Utc> = early.parse().unwrap();
        seed_observability_facts(&database.pool, end).await.unwrap();
        seed_calculated_billing_facts(&database.pool, end)
            .await
            .unwrap();
        sqlx::query(
            "update model_requests set started_at = $1, completed_at = $1 + interval '1 second',
               downstream_committed_at = $1 where id = 'req_observe_success'",
        )
        .bind(early)
        .execute(&database.pool)
        .await
        .unwrap();
        let range =
            admin_observability::TimeRange::calendar_at(CalendarPeriod::Today, end, timezone)
                .unwrap();
        let dashboard = admin_observability_store(&database.pool)
            .with_timezone(timezone)
            .dashboard_summary(
                admin_observability::DashboardQuery::new(admin_observability::TimeRange {
                    start: range.start,
                    end: range.end,
                }),
                end,
            )
            .await
            .expect("calendar dashboard supports variable day lengths");
        assert_eq!(
            dashboard
                .account_usage
                .iter()
                .map(|account| account.request_count)
                .sum::<u64>(),
            metrics_count,
            "{name} {end}"
        );
        let accounts = observability_repository(&database.pool)
            .provider_account_usage(
                ProviderAccountUsageQuery::for_accounts(
                    ObservabilityRange {
                        start: range.start,
                        end,
                    },
                    vec!["acct_observe".to_owned()],
                )
                .unwrap()
                .with_hourly_request_buckets()
                .unwrap(),
            )
            .await
            .expect("hourly buckets are separate from calendar metrics");
        assert_eq!(accounts[0].request_count, metrics_count, "{name}");
        let buckets = &accounts[0].request_buckets;
        assert_eq!(buckets.len(), 24);
        assert_eq!(
            buckets
                .iter()
                .map(|bucket| bucket.request_count)
                .sum::<u64>(),
            hourly_count,
            "{name}"
        );
        let current_hour =
            chrono::DateTime::from_timestamp(end.timestamp().div_euclid(3600) * 3600, 0).unwrap();
        for (index, bucket) in buckets.iter().enumerate() {
            assert_eq!(
                bucket.bucket_start,
                current_hour - TimeDelta::hours(23 - index as i64)
            );
        }
        if end == current_hour {
            assert_eq!(buckets.last().unwrap().request_count, 0);
        }
        database.close().await;
    }
}

#[tokio::test]
async fn dashboard_shares_account_facts_and_bounds_slow_cooldown_reads() {
    use std::sync::atomic::Ordering::Relaxed;
    let Some(database) = TestDatabase::create("dashboard_shared_state").await else {
        return;
    };
    let Some(redis_url) = crate::support::test_env("CPR_TEST_REDIS_URL") else {
        database.close().await;
        return;
    };
    use gateway_store::redis::{
        CredentialBoundedLeaseAcquisition, CredentialBoundedLeaseRequest,
        CredentialLeaseRepository as _, CredentialLeaseScope, RedisCredentialLeaseRepository,
    };
    let connection = redis::Client::open(redis_url)
        .unwrap()
        .get_connection_manager()
        .await
        .unwrap();
    let runtime = RedisCredentialLeaseRepository::new(
        connection,
        &format!("dashboard-shared-{}", uuid::Uuid::new_v4()),
    )
    .unwrap();
    let request = CredentialBoundedLeaseRequest {
        scope: CredentialLeaseScope::ProviderAccount,
        resource_id: "acct_shared_1".to_owned(),
        owner_id: "dashboard-test".to_owned(),
        max_concurrent: 3,
        request_interval: Duration::ZERO,
        ttl: Duration::from_secs(60),
    };
    let CredentialBoundedLeaseAcquisition::Acquired(lease) =
        runtime.try_acquire_bounded_lease(&request).await.unwrap()
    else {
        panic!("test lease capacity")
    };
    let now = Utc::now();
    sqlx::query(
        "insert into provider_accounts (
            id, provider_kind, name, upstream_user_id, authentication_kind,
            provider_credentials_json, credential_revision, has_refresh_token,
            enabled, concurrency_limit, credential_state, quota_access_state,
            quota_access_observed_at, credential_observed_at, created_at, updated_at
         ) select 'acct_shared_' || n, 'openai', 'account ' || n, 'user-shared-' || n, 'oauth',
                  '{}'::jsonb, 1, false, true, case when n % 2 = 0 then 3 else null end,
                  'ready', 'allowed', $1, $1, $1, $1
           from generate_series(1, 1000) n",
    )
    .bind(now)
    .execute(&database.pool)
    .await
    .unwrap();
    let mut cooldowns = StaticCooldowns::new([]);
    cooldowns.delay = Duration::from_millis(3);
    let cooldowns = Arc::new(cooldowns);
    let store = gateway_store::postgres::PgAdminObservabilityStore::new(
        database.pool.clone(),
        Some(runtime),
        Some(cooldowns.clone()),
        observability_query_budget(),
    );
    let started = std::time::Instant::now();
    let observation = store
        .dashboard_summary(
            admin_observability::DashboardQuery::new(
                admin_observability::TimeRange::new(now - TimeDelta::hours(1), now).unwrap(),
            ),
            now,
        )
        .await
        .unwrap();
    assert_eq!(observation.provider_accounts.normal, 1000);
    let slots = observation.runtime_slots.unwrap();
    assert_eq!(
        (
            slots.inherited_accounts,
            slots.overridden_slots,
            slots.used_slots
        ),
        (500, 1500, Some(1))
    );
    assert_eq!(
        cooldowns.reads.load(Relaxed),
        1000,
        "summary and slots share one read per account"
    );
    let peak = cooldowns.peak.load(Relaxed);
    assert!(
        (2..=128).contains(&peak),
        "slow reads must be concurrent and bounded: {peak}"
    );
    assert_eq!(cooldowns.active.load(Relaxed), 0);
    eprintln!(
        "dashboard 1000 accounts, 3 ms cooldown delay: peak={peak}, elapsed={:?}",
        started.elapsed()
    );
    lease.release().await.unwrap();
    database.close().await;
}

#[tokio::test]
async fn observability_store_executes_caller_selection_without_replacing_policy() {
    use admin_observability::{
        DashboardQuery, Granularity, RequestOutcome, TimeRange, UsageFilter,
    };
    let Some(database) = TestDatabase::create("observability_caller_selection").await else {
        return;
    };
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-07T12:30:00Z")
        .unwrap()
        .with_timezone(&Utc);
    seed_observability_facts(&database.pool, now).await.unwrap();
    seed_calculated_billing_facts(&database.pool, now)
        .await
        .unwrap();
    sqlx::query(
        "insert into provider_accounts (
            id, provider_kind, name, upstream_user_id, authentication_kind,
            provider_credentials_json, credential_revision, has_refresh_token,
            enabled, credential_state, credential_observed_at, created_at, updated_at
         ) values ('acct_other', 'openai', 'other', 'user-other', 'oauth',
                   '{}'::jsonb, 1, false, true, 'ready', $1, $1, $1)",
    )
    .bind(now)
    .execute(&database.pool)
    .await
    .unwrap();
    let range = TimeRange::new(now - TimeDelta::hours(1), now + TimeDelta::hours(1)).unwrap();
    let store = admin_observability_store(&database.pool);
    let mut query = DashboardQuery::new(range);
    query.account_limit = 1;
    query.recent_request_limit = 1;
    query.recent_request_filter = UsageFilter::default();
    query.granularity = Granularity::Hour;
    let dashboard = store.dashboard_summary(query.clone(), now).await.unwrap();
    assert_eq!(dashboard.account_usage.len(), 1);
    assert_eq!(dashboard.recent_requests.len(), 1);
    assert_eq!(dashboard.trend.len(), 3);
    assert!(
        dashboard
            .trend
            .iter()
            .all(|point| point.granularity == Granularity::Hour)
    );
    query.recent_request_filter.request_id = Some("req_observe_calculated".to_owned());
    let selected = store.dashboard_summary(query.clone(), now).await.unwrap();
    assert_eq!(selected.recent_requests.len(), 1);
    assert_eq!(selected.recent_requests[0].id, "req_observe_calculated");
    // 用量列表仍只展示完成交付的事实，传入筛选不能扩大这条持久化合同
    query.recent_request_filter = UsageFilter {
        outcome: Some(RequestOutcome::Failed),
        ..UsageFilter::default()
    };
    assert!(
        store
            .dashboard_summary(query, now)
            .await
            .unwrap()
            .recent_requests
            .is_empty()
    );
    for granularity in [Granularity::Hour, Granularity::Day] {
        let trend = store
            .usage_trend(range, UsageFilter::default(), granularity)
            .await
            .unwrap();
        let facts = store
            .usage_calculated_billing_facts(range, UsageFilter::default(), granularity)
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(facts.len(), 1);
        assert!(trend.iter().all(|point| point.granularity == granularity));
        assert!(
            trend
                .iter()
                .any(|point| point.bucket_start == facts[0].bucket_start
                    && point.metrics.request_count > 0)
        );
    }
    let diagnostics = store
        .usage_diagnostics(
            range,
            UsageFilter::default(),
            DiagnosticDimension::Status,
            1,
        )
        .await
        .unwrap();
    assert_eq!(diagnostics.items.len(), 1);
    assert_eq!(diagnostics.total_request_count, 5);
    for invalid_limit in [0, 101] {
        assert!(
            store
                .usage_diagnostics(
                    range,
                    UsageFilter::default(),
                    DiagnosticDimension::Status,
                    invalid_limit
                )
                .await
                .is_err()
        );
    }
    database.close().await;
}
