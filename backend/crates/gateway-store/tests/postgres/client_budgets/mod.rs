//! 测试模块入口与共享夹具

use std::{collections::BTreeMap, time::SystemTime};

use chrono::{DateTime, Utc};
use gateway_admin::{
    model::{
        MutationActor, MutationContext, Revision,
        client_keys::{
            ClientKeyBudgetMutationOrigin, ClientKeyBudgetPeriod, DeleteClientKey, NewClientKey,
            ResetClientKeyBudget, UpdateClientKey, UpdateClientKeyBudgetLimits,
        },
        plugin_resources::PluginResourceOwner,
        plugins::{PluginSource, instances::PluginInstance},
    },
    ports::{
        plugins::PluginStore as _,
        store::{AdminStoreErrorKind, ClientKeyStore as _},
    },
};
use gateway_core::{
    engine::{
        ModelRequestId,
        budget::{ClientBudgetCharge, ClientBudgetPort, ClientBudgetStatus},
    },
    error::GatewayErrorKind,
    policy::{ClientApiKeyId, RateLimits},
};
use gateway_store::postgres::{
    ClientApiKeyRepository as _, PgAdminClientKeyStore, PgClientApiKeyRepository,
    PgClientBudgetStore, PgPluginStore,
};

use super::TestDatabase;

pub(super) fn key_id(key: &str) -> ClientApiKeyId {
    ClientApiKeyId::new(key).unwrap()
}

pub(super) fn charge(key: &str, request: &str, amount: &str) -> ClientBudgetCharge {
    ClientBudgetCharge {
        key_id: key_id(key),
        request_id: ModelRequestId::new(format!("req_{request}")).unwrap(),
        amount_usd: amount.parse().unwrap(),
        completed_at: SystemTime::now(),
    }
}

pub(super) async fn seed(database: &TestDatabase, key: &str, daily: &str, weekly: &str) {
    sqlx::query(
        "insert into client_api_keys (id, name, key, daily_limit_usd, weekly_limit_usd, created_at, updated_at)
        values ($1, $1, $2, $3::text::numeric, $4::text::numeric, now(), now())",
    )
    .bind(key)
    .bind(format!("sk_{key:a<43}"))
    .bind(daily)
    .bind(weekly)
    .execute(&database.pool)
    .await
    .unwrap();
}

pub(super) async fn status(database: &TestDatabase, key: &str) -> ClientBudgetStatus {
    PgClientApiKeyRepository::new(database.pool.clone())
        .get_client_api_key(key)
        .await
        .unwrap()
        .unwrap()
        .budget
}

pub(super) fn context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "budget-test".to_owned(),
    }
}

async fn plugin_reset_owner(database: &TestDatabase) -> PluginResourceOwner {
    super::plugins::artifacts::initialize_revision(database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let package = super::plugins::artifacts::artifact('b', &["linux-x86_64"]);
    let installed = store
        .install_artifact(package, PluginSource::Upload, &context())
        .await
        .unwrap();
    let accepted = store
        .accept_artifact(&installed.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let instance = store
        .save_instance(
            PluginInstance {
                id: uuid::Uuid::now_v7().to_string(),
                name: "budget reset".into(),
                artifact_sha256: installed.artifact.metadata.sha256,
                enabled: true,
                trusted_process: true,
                configuration: serde_json::json!({}),
                secrets: BTreeMap::new(),

                bindings: vec![],
                revision: Revision::new(1).unwrap(),
            },
            accepted.config_revision,
            &context(),
        )
        .await
        .unwrap()
        .instance;
    PluginResourceOwner {
        instance_id: instance.id,
        artifact_sha256: instance.artifact_sha256,
        revision: instance.revision,
    }
}

fn budget_update(
    key: &str,
    daily: Option<&str>,
    weekly: Option<&str>,
) -> UpdateClientKeyBudgetLimits {
    UpdateClientKeyBudgetLimits {
        id: key_id(key),
        daily_limit_usd: daily.map(|value| value.parse().unwrap()),
        weekly_limit_usd: weekly.map(|value| value.parse().unwrap()),
    }
}

mod buffer;
mod windows;

mod store;
