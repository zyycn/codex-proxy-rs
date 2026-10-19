//! PostgreSQL 适配测试入口与隔离数据库夹具

use std::{str::FromStr, time::Duration};

use gateway_store::postgres::{
    ObservabilityQueryBudget, PgAdminAccountStore, PgAdminObservabilityStore,
    PgObservabilityRepository, connect_and_migrate,
};
use sqlx::{
    ConnectOptions as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

mod account_groups;
mod admin_security_audit;
mod admission_recovery;
mod analytics;
mod analytics_buckets;
mod analytics_reset;
mod backup;
mod client_budgets;
mod client_keys;
mod connection;
mod control_plane;
mod execution;
mod execution_buffer;
mod health;
mod observability;
mod ops_events;
mod plugins;
mod pricing;
mod provider_accounts;
mod proxies;
mod query_budget;
mod retention;
mod runtime_settings;
mod schema_integrity;
mod snapshot;
mod snapshots;
mod usage_data;
mod usage_paging;

static TEST_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

pub(super) struct TestDatabase {
    admin: PgPool,
    pub(super) pool: PgPool,
    schema: String,
}

pub(super) fn observability_query_budget() -> ObservabilityQueryBudget {
    ObservabilityQueryBudget::try_new(4, Duration::from_secs(1), Duration::from_secs(10))
        .expect("valid test observability query budget")
}

pub(super) fn observability_repository(pool: &PgPool) -> PgObservabilityRepository {
    PgObservabilityRepository::new(pool.clone(), None, observability_query_budget(), None)
}

pub(super) fn admin_observability_store(pool: &PgPool) -> PgAdminObservabilityStore {
    PgAdminObservabilityStore::new(pool.clone(), None, None, observability_query_budget())
}

pub(super) fn admin_account_store(pool: &PgPool) -> PgAdminAccountStore {
    PgAdminAccountStore::new(pool.clone(), None, observability_query_budget())
}

impl TestDatabase {
    pub(super) async fn create(label: &str) -> Option<Self> {
        let database_url = crate::support::test_env("CPR_TEST_DATABASE_URL")?;
        let schema = format!("cpr_store_{label}_{}", Uuid::new_v4().simple());
        // 临时 schema 验证事务可见性与回滚，不模拟 PostgreSQL 掉电恢复
        // 仅这些测试连接异步刷 WAL，保持服务端配置和生产连接行为不变
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse test PostgreSQL URL")
            .options([("synchronous_commit", "off")]);
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .expect("connect test PostgreSQL");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create schema \"{schema}\"")))
            .execute(&admin)
            .await
            .expect("create test schema");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.options([("search_path", schema.as_str())]))
            .await
            .expect("connect isolated test schema");
        TEST_MIGRATOR
            .run(&pool)
            .await
            .expect("apply test migrations");
        Some(Self {
            admin,
            pool,
            schema,
        })
    }

    pub(super) async fn close(self) {
        self.pool.close().await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "drop schema \"{}\" cascade",
            self.schema
        )))
        .execute(&self.admin)
        .await
        .expect("drop test schema");
        self.admin.close().await;
    }
}

/// 领域合同测试显式等待后台投影，异步一致性测试直接控制 Worker
pub(super) async fn drain_analytics(pool: &PgPool) {
    let worker = gateway_store::postgres::PgRequestAnalyticsRepository::new(
        pool.clone(),
        observability_query_budget(),
    )
    .with_billing_projector(std::sync::Arc::new(TestBillingProjector));
    for _ in 0..100 {
        worker
            .maintain(1000)
            .await
            .expect("maintain test analytics");
        let status = worker.status().await.expect("test analytics status");
        if !status.building && status.pending_requests == 0 && status.pending_buckets == 0 {
            return;
        }
    }
    panic!("test analytics did not converge");
}

struct TestBillingProjector;

impl gateway_admin::ports::store::UsageBillingProjector for TestBillingProjector {
    fn project(
        &self,
        fact: &gateway_admin::model::observability::UsageCalculatedBillingFact,
    ) -> Result<
        gateway_admin::model::observability::UsageBillingProjection,
        gateway_admin::model::AdminError,
    > {
        Ok(
            gateway_admin::model::observability::UsageBillingProjection {
                standard_amount: Some(fact.total.amount.clone()),
                no_cache_amount: Some(fact.total.amount.clone()),
            },
        )
    }
}
