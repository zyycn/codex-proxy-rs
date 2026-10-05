//! 验证插件观察回调的类型化状态读写与命名空间约束

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use gateway_admin::{
    model::{
        Revision,
        plugins::{
            instances::{
                PluginCapabilityBinding, PluginFailurePolicy, PluginInstance,
                PluginInstanceSnapshot,
            },
            state::{
                ApplyPluginStateMigration, DeletePluginState, PluginStateConfiguration,
                PluginStateMigrationBatch, PluginStateNamespaceOwner, PluginStateOwner,
                PluginStateOwnerRequest, PluginStateRecord, PluginStateTransition,
                PluginStateWrite, PutPluginState,
            },
        },
    },
    ports::plugins::{
        PluginPackageInspector, PluginStateStore, PluginStateStoreError, PluginStateStoreErrorKind,
        PluginStateStoreResult,
    },
};
use gateway_core::{
    engine::{
        ModelRequestId,
        observation::{RequestObservation, RequestObservationOutcome},
    },
    identity::ProviderKind,
    operation::OperationKind,
    routing::{ConfigRevision, PublicModelId},
    runtime::extensions::ExtensionPreparationPort,
    upstream::UpstreamSendState,
};
use gateway_plugin_runtime::{
    PackageInspector, PackageLimits, PluginRuntime, PluginRuntimeConfig, RpcLimits,
};
use gateway_plugin_sdk::{Capability, Contributions, Stage, StateNamespace};
use serde_json::json;

use crate::support::store::Store;

struct StoredRecord {
    value: serde_json::Value,
    version: u64,
}

#[derive(Default)]
struct StateStore {
    records: Mutex<BTreeMap<(String, String), StoredRecord>>,
    calls: Mutex<Vec<String>>,
    next_version: AtomicU64,
}

impl StateStore {
    fn error(kind: PluginStateStoreErrorKind) -> PluginStateStoreError {
        PluginStateStoreError::new(kind)
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn is_empty(&self) -> bool {
        self.records.lock().unwrap().is_empty()
    }
}

#[async_trait]
impl PluginStateStore for StateStore {
    async fn load_owner(
        &self,
        request: PluginStateOwnerRequest,
    ) -> PluginStateStoreResult<Option<PluginStateOwner>> {
        let namespaces = request
            .configuration
            .namespaces
            .iter()
            .map(|schema| {
                (
                    schema.namespace.clone(),
                    PluginStateNamespaceOwner::from_store(
                        format!("generation-{}", schema.namespace),
                        format!("fence-{}", schema.namespace),
                        schema.schema_version,
                    ),
                )
            })
            .collect();
        Ok(Some(PluginStateOwner::from_store(
            request.instance_id,
            request.artifact_sha256,
            request.instance_revision,
            namespaces,
        )))
    }

    async fn get(
        &self,
        owner: &PluginStateOwner,
        namespace: &str,
        key: &str,
    ) -> PluginStateStoreResult<Option<PluginStateRecord>> {
        let namespace_owner = owner
            .namespace(namespace)
            .ok_or_else(|| Self::error(PluginStateStoreErrorKind::PermissionDenied))?;
        self.calls.lock().unwrap().push(format!("get:{namespace}"));
        Ok(self
            .records
            .lock()
            .unwrap()
            .get(&(namespace.into(), key.into()))
            .map(|record| PluginStateRecord {
                key: key.into(),
                value: record.value.clone(),
                version: record.version,
                schema_version: namespace_owner.schema_version(),
            }))
    }

    async fn put(
        &self,
        owner: &PluginStateOwner,
        command: PutPluginState,
    ) -> PluginStateStoreResult<PluginStateWrite> {
        owner
            .namespace(&command.namespace)
            .ok_or_else(|| Self::error(PluginStateStoreErrorKind::PermissionDenied))?;
        let key = (command.namespace.clone(), command.key.clone());
        let mut records = self.records.lock().unwrap();
        let current = records.get(&key).map(|record| record.version);
        if command.expected_version != current {
            return Err(Self::error(PluginStateStoreErrorKind::Conflict));
        }
        let version = self.next_version.fetch_add(1, Ordering::Relaxed) + 1;
        records.insert(
            key,
            StoredRecord {
                value: command.value,
                version,
            },
        );
        self.calls
            .lock()
            .unwrap()
            .push(format!("put:{}", command.namespace));
        Ok(PluginStateWrite { version })
    }

    async fn delete(
        &self,
        owner: &PluginStateOwner,
        command: DeletePluginState,
    ) -> PluginStateStoreResult<bool> {
        owner
            .namespace(&command.namespace)
            .ok_or_else(|| Self::error(PluginStateStoreErrorKind::PermissionDenied))?;
        let key = (command.namespace.clone(), command.key);
        let mut records = self.records.lock().unwrap();
        let Some(current) = records.get(&key) else {
            return Ok(false);
        };
        if current.version != command.expected_version {
            return Err(Self::error(PluginStateStoreErrorKind::Conflict));
        }
        records.remove(&key);
        self.calls
            .lock()
            .unwrap()
            .push(format!("delete:{}", command.namespace));
        Ok(true)
    }

    async fn transition_required(
        &self,
        _: &str,
        _: &PluginStateConfiguration,
    ) -> PluginStateStoreResult<bool> {
        Ok(false)
    }

    async fn begin_transition(
        &self,
        _: &str,
        _: Revision,
        _: &str,
        _: PluginStateConfiguration,
    ) -> PluginStateStoreResult<PluginStateTransition> {
        Err(Self::error(PluginStateStoreErrorKind::Unavailable))
    }

    async fn migration_batch(
        &self,
        _: &str,
        _: &str,
        _: u32,
    ) -> PluginStateStoreResult<PluginStateMigrationBatch> {
        Err(Self::error(PluginStateStoreErrorKind::Unavailable))
    }

    async fn apply_migration_batch(
        &self,
        _: ApplyPluginStateMigration,
    ) -> PluginStateStoreResult<()> {
        Err(Self::error(PluginStateStoreErrorKind::Unavailable))
    }

    async fn abort_transition(&self, _: &str) -> PluginStateStoreResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn observation_callbacks_use_typed_state_crud_and_reject_undeclared_namespaces() {
    let cache = tempfile::tempdir().unwrap();
    let marker = cache.path().join("state.jsonl");
    let contributes = Contributions::from([crate::support::contribution(
        Capability::Observer,
        vec![Stage::Observation],
        vec![],
        vec![],
    )]);
    let state_namespace = StateNamespace {
        namespace: "cache".into(),
        schema_version: 1,
        schema: json!({
            "type": "object",
            "required": ["value"],
            "properties": {"value": {"type": "integer"}},
            "additionalProperties": false,
        }),
        maximum_records: 8,
        maximum_bytes: 1024,
        maximum_value_bytes: 128,
        migrates_from: vec![],
    };
    let archive = crate::support::package_with_contributions_and_state(
        crate::support::worker(),
        contributes,
        vec![state_namespace],
    );
    let artifact = PackageInspector::new(PackageLimits::default(), "1.0.0".parse().unwrap())
        .inspect(archive, None)
        .await
        .unwrap();
    let digest = artifact.metadata.sha256.clone();
    let instance = PluginInstance {
        id: "state-instance".into(),
        name: "state-instance".into(),
        artifact_sha256: digest.clone(),
        enabled: true,
        trusted_process: true,
        configuration: json!({
            "state_fixture": {
                "namespace": "cache",
                "denied_namespace": "private",
                "key": "request",
                "value": {"value": 7},
            },
            "state_marker": marker,
        }),
        secrets: BTreeMap::new(),

        bindings: vec![PluginCapabilityBinding {
            contribution: "test.example.observer".into(),
            stage: "observation".into(),
            order: 0,
            failure_policy: PluginFailurePolicy::Observe,
            client_key_ids: vec![],
            account_group_ids: vec![],
            provider_ids: vec!["openai".into()],
            models: vec!["gpt-state".into()],
            event: Some("request_completed".into()),
            identity_bindings: vec![],
        }],
        revision: Revision::new(1).unwrap(),
    };
    let store = Arc::new(Store {
        artifacts: BTreeMap::from([(digest, artifact)]),
        snapshot: Mutex::new(PluginInstanceSnapshot {
            config_revision: Revision::new(1).unwrap(),
            instances: vec![instance],
        }),
    });
    let state = Arc::new(StateStore::default());
    let runtime = PluginRuntime::new(
        store,
        state.clone(),
        PluginRuntimeConfig {
            cache_directory: cache.path().join("packages"),
            host_version: "1.0.0".parse().unwrap(),
            package_limits: PackageLimits::default(),
            rpc_limits: RpcLimits::default(),
            restart_circuit: Default::default(),
        },
        Arc::new(gateway_host::outbound::HttpClient::new().unwrap()),
        Arc::new(gateway_host::process::ProcessSupervisor::new(
            std::num::NonZeroUsize::new(16).unwrap(),
        )),
    );
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let plan = runtime.observer_registry().resolve(&generation).unwrap();
    plan.dispatch(
        generation.clone(),
        RequestObservation::new(
            ModelRequestId::new("req_state").unwrap(),
            ConfigRevision::new(1).unwrap(),
            gateway_core::engine::observation::RequestObservationScope::new(
                gateway_core::policy::ClientApiKeyId::new("client-key-state").unwrap(),
                vec![],
            ),
            OperationKind::Generate,
            RequestObservationOutcome::Succeeded,
            UpstreamSendState::Sent,
            SystemTime::now(),
        )
        .with_requested_model(PublicModelId::new("gpt-state").unwrap())
        .with_provider(ProviderKind::new("openai").unwrap()),
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state.calls(), ["put:cache", "get:cache", "delete:cache"]);
    assert!(state.is_empty());
    let marker = std::fs::read_to_string(marker).unwrap();
    assert!(marker.contains("\"version\":1"));
    assert!(marker.contains("\"schema_version\":1"));

    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path().join("packages").as_path()).await;
}
