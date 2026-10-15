//! 公开原生用量读写与纯计价，插件拥有跨调用流程和恢复策略

mod pricing;
mod validation;

use crate::{
    model::{
        auth::{AdminAuditEvent, AuditActorKind},
        usage_data::*,
    },
    ports::{provider::ProviderAdminRegistry, store::SettingsStore, usage_data::*},
};
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait UsageDataService: Send + Sync {
    async fn list_aggregates(
        &self,
        request: ListUsageAggregates,
    ) -> UsageDataResult<UsageAggregatePage>;
    async fn write_aggregates(
        &self,
        actor: &UsageActor,
        request: WriteUsageAggregates,
    ) -> UsageDataResult<UsageAggregateWriteResult>;
    async fn get_record(&self, request: GetUsageRecord) -> UsageDataResult<UsageRecord>;
    async fn list_records(&self, request: ListUsageRecords) -> UsageDataResult<UsageRecordPage>;
    async fn write_records(
        &self,
        actor: &UsageActor,
        request: WriteUsageRecords,
    ) -> UsageDataResult<UsageWriteResult>;
    async fn evaluate_pricing(
        &self,
        request: EvaluateUsagePricing,
    ) -> UsageDataResult<UsagePricingEvaluation>;
}

pub struct DefaultUsageDataService {
    store: Arc<dyn UsageDataStore>,
    settings: Arc<dyn SettingsStore>,
    providers: ProviderAdminRegistry,
}
impl DefaultUsageDataService {
    pub fn new(
        store: Arc<dyn UsageDataStore>,
        settings: Arc<dyn SettingsStore>,
        providers: ProviderAdminRegistry,
    ) -> Self {
        Self {
            store,
            settings,
            providers,
        }
    }
}

#[async_trait]
impl UsageDataService for DefaultUsageDataService {
    async fn list_aggregates(
        &self,
        request: ListUsageAggregates,
    ) -> UsageDataResult<UsageAggregatePage> {
        validation::query(&ListUsageRecords {
            filter: request.filter.clone(),
            cursor: request.cursor.clone(),
            limit: request.limit,
        })?;
        self.store.list_aggregates(request).await
    }
    async fn write_aggregates(
        &self,
        actor: &UsageActor,
        request: WriteUsageAggregates,
    ) -> UsageDataResult<UsageAggregateWriteResult> {
        validation::bounded(&request, request.changes.len())?;
        let mut changes = Vec::with_capacity(request.changes.len());
        for change in request.changes {
            validation::aggregate(&change)?;
            let identity = match &change {
                UsageAggregateChange::Put { aggregate } => &aggregate.identity,
                UsageAggregateChange::Delete { identity } => identity,
            };
            let action = match &change {
                UsageAggregateChange::Put { .. } => "usage.aggregate.put",
                UsageAggregateChange::Delete { .. } => "usage.aggregate.delete",
            };
            let audit = audit_event(
                actor,
                action,
                "usage_aggregate",
                &identity.key,
                vec!["aggregate".into()],
            );
            changes.push((change, audit));
        }
        self.store
            .write_aggregates(StoredAggregateWrite { changes })
            .await
    }
    async fn get_record(&self, request: GetUsageRecord) -> UsageDataResult<UsageRecord> {
        validation::text(&request.id, 128)?;
        self.store.get_record(request).await
    }
    async fn list_records(&self, request: ListUsageRecords) -> UsageDataResult<UsageRecordPage> {
        validation::query(&request)?;
        self.store.list_records(request).await
    }
    async fn write_records(
        &self,
        actor: &UsageActor,
        request: WriteUsageRecords,
    ) -> UsageDataResult<UsageWriteResult> {
        validation::bounded(&request, request.changes.len())?;
        let mut changes = Vec::with_capacity(request.changes.len());
        for change in request.changes {
            validation::change(&change)?;
            let action = match &change {
                UsageRecordChange::Put { .. } => "usage.put",
                UsageRecordChange::Update { .. } => "usage.update",
                UsageRecordChange::Delete { .. } => "usage.delete",
            };
            let fields = match &change {
                UsageRecordChange::Update { tokens, cost, .. } => tokens
                    .as_ref()
                    .map(|_| "tokens".to_owned())
                    .into_iter()
                    .chain(cost.as_ref().map(|_| "cost".to_owned()))
                    .collect(),
                _ => vec!["record".into()],
            };
            let audit = audit_event(actor, action, "model_request", change.id(), fields);
            changes.push((change, audit));
        }
        self.store.write_records(StoredUsageWrite { changes }).await
    }
    async fn evaluate_pricing(
        &self,
        request: EvaluateUsagePricing,
    ) -> UsageDataResult<UsagePricingEvaluation> {
        validation::bounded(&request, request.items.len())?;
        let stored = self
            .settings
            .load_pricing()
            .await
            .map_err(|_| UsageDataError::unavailable("读取价目失败"))?;
        let prices = gateway_core::metering::merge_pricing(stored.synced, &stored.overrides);
        let mut items = Vec::with_capacity(request.items.len());
        for input in request.items {
            validation::text(&input.id, 128)?;
            items.push(self.evaluate_one(input, &prices)?);
        }
        Ok(UsagePricingEvaluation { items })
    }
}

fn audit_event(
    actor: &UsageActor,
    action: &str,
    entity_kind: &str,
    id: &str,
    changed_fields: Vec<String>,
) -> AdminAuditEvent {
    let actor_ref = match actor {
        UsageActor::Plugin {
            instance_id,
            publisher,
            name,
            ..
        } => format!("plugin:{publisher}/{name}:{instance_id}"),
        UsageActor::Admin { subject } => format!("admin:{subject}"),
        UsageActor::System => "system".into(),
    };
    AdminAuditEvent {
        id: format!("audit_{}", uuid::Uuid::now_v7().simple()),
        actor_kind: AuditActorKind::System,
        actor_admin_user_id: None,
        actor_ref,
        request_id: None,
        action: action.into(),
        entity_kind: entity_kind.into(),
        entity_ref: id.to_owned(),
        config_revision: None,
        changed_fields,
        occurred_at: chrono::Utc::now(),
    }
}
