//! 插件私有状态回调的访问授权、数据校验与存储调用

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use gateway_admin::{
    model::{
        AdminError,
        plugins::{
            instances::PluginInstance,
            state::{
                DeletePluginState, PluginStateConfiguration, PluginStateOwner,
                PluginStateOwnerRequest, PluginStateSchema, PutPluginState,
            },
        },
    },
    ports::plugins::{PluginStateStore, PluginStateStoreError, PluginStateStoreErrorKind},
};
use gateway_plugin_sdk::{
    ErrorCode, Manifest, PluginFault,
    call::host::{
        StateDeleteRequest, StateDeleteResult, StateGetRequest, StateGetResult, StatePutRequest,
        StatePutResult, StateRecord,
    },
};
use sha2::{Digest as _, Sha256};

use crate::RpcReply;

pub(crate) struct PluginPrivateState {
    store: Arc<dyn PluginStateStore>,
    configuration: PluginStateConfiguration,
    validators: BTreeMap<String, jsonschema::Validator>,
    owner: RwLock<Option<PluginStateOwner>>,
}

impl PluginPrivateState {
    pub(crate) fn new(
        store: Arc<dyn PluginStateStore>,
        manifest: &Manifest,
        configuration: PluginStateConfiguration,
        owner: Option<PluginStateOwner>,
    ) -> Result<Self, AdminError> {
        let validators = validators(manifest)?;
        Ok(Self {
            store,
            configuration,
            validators,
            owner: RwLock::new(owner),
        })
    }

    pub(crate) fn owner_request(&self, instance: &PluginInstance) -> PluginStateOwnerRequest {
        PluginStateOwnerRequest {
            instance_id: instance.id.clone(),
            artifact_sha256: instance.artifact_sha256.clone(),
            instance_revision: instance.revision,
            configuration: self.configuration.clone(),
        }
    }

    pub(crate) fn activate(&self, owner: PluginStateOwner) {
        *self
            .owner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(owner);
    }

    pub(crate) fn validate_value(
        &self,
        namespace: &str,
        value: &serde_json::Value,
    ) -> Result<(), AdminError> {
        let validator = self
            .validators
            .get(namespace)
            .ok_or_else(|| AdminError::invalid("插件状态迁移使用了未声明的命名空间"))?;
        if !validator.is_valid(value) {
            return Err(AdminError::invalid("插件状态迁移结果不符合目标 schema"));
        }
        Ok(())
    }

    pub(crate) async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        if !payload.is_empty() {
            return Err(invalid_fault());
        }
        let owner = self
            .owner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(denied_fault)?;
        let result = match method {
            "host.state.get" => {
                let request: StateGetRequest =
                    serde_json::from_value(params).map_err(|_| invalid_fault())?;
                self.authorize(&request.namespace)?;
                let record = self
                    .store
                    .get(&owner, &request.namespace, &request.key)
                    .await
                    .map_err(map_store_fault)?
                    .map(|record| StateRecord {
                        value: record.value,
                        version: record.version,
                        schema_version: record.schema_version,
                    });
                serde_json::to_value(StateGetResult { record }).map_err(|_| internal_fault())?
            }
            "host.state.put" => {
                let request: StatePutRequest =
                    serde_json::from_value(params).map_err(|_| invalid_fault())?;
                self.authorize(&request.namespace)?;
                self.validators
                    .get(&request.namespace)
                    .filter(|validator| validator.is_valid(&request.value))
                    .ok_or_else(invalid_fault)?;
                let write = self
                    .store
                    .put(
                        &owner,
                        PutPluginState {
                            namespace: request.namespace,
                            key: request.key,
                            value: request.value,
                            expected_version: request.expected_version,
                        },
                    )
                    .await
                    .map_err(map_store_fault)?;
                serde_json::to_value(StatePutResult {
                    version: write.version,
                })
                .map_err(|_| internal_fault())?
            }
            "host.state.delete" => {
                let request: StateDeleteRequest =
                    serde_json::from_value(params).map_err(|_| invalid_fault())?;
                self.authorize(&request.namespace)?;
                let deleted = self
                    .store
                    .delete(
                        &owner,
                        DeletePluginState {
                            namespace: request.namespace,
                            key: request.key,
                            expected_version: request.expected_version,
                        },
                    )
                    .await
                    .map_err(map_store_fault)?;
                serde_json::to_value(StateDeleteResult { deleted }).map_err(|_| internal_fault())?
            }
            _ => return Err(denied_fault()),
        };
        Ok(RpcReply {
            result,
            payload: Vec::new(),
        })
    }

    fn authorize(&self, namespace: &str) -> Result<(), PluginFault> {
        if !self.validators.contains_key(namespace) {
            return Err(denied_fault());
        }
        Ok(())
    }
}

pub(crate) fn configuration(manifest: &Manifest) -> Result<PluginStateConfiguration, AdminError> {
    let _ = validators(manifest)?;
    let mut namespaces = Vec::with_capacity(manifest.state.len());
    for state in &manifest.state {
        let mut schema = state.schema.clone();
        schema.sort_all_objects();
        let encoded = serde_json::to_vec(&schema)
            .map_err(|_| AdminError::invalid("插件状态 schema 无法编码"))?;
        namespaces.push(PluginStateSchema {
            namespace: state.namespace.clone(),
            schema_version: state.schema_version,
            schema_sha256: hex::encode(Sha256::digest(encoded)),
            schema: state.schema.clone(),
            maximum_records: state.maximum_records,
            maximum_bytes: state.maximum_bytes,
            maximum_value_bytes: state.maximum_value_bytes,
            migrates_from: state.migrates_from.clone(),
        });
    }
    namespaces.sort_by(|left, right| left.namespace.cmp(&right.namespace));
    Ok(PluginStateConfiguration { namespaces })
}

fn validators(manifest: &Manifest) -> Result<BTreeMap<String, jsonschema::Validator>, AdminError> {
    manifest
        .state
        .iter()
        .map(|state| {
            let validator = jsonschema::options()
                .offline()
                .with_pattern_options(
                    jsonschema::PatternOptions::fancy_regex().backtrack_limit(20_000),
                )
                .build(&state.schema)
                .map_err(|_| AdminError::invalid("插件状态 schema 无效或引用了外部资源"))?;
            Ok((state.namespace.clone(), validator))
        })
        .collect()
}

fn map_store_fault(error: PluginStateStoreError) -> PluginFault {
    let (code, message) = match error.kind() {
        PluginStateStoreErrorKind::Invalid => (ErrorCode::InvalidInput, "state input is invalid"),
        PluginStateStoreErrorKind::Conflict => (ErrorCode::Conflict, "state version conflicts"),
        PluginStateStoreErrorKind::Quota => (ErrorCode::Capacity, "state quota is exhausted"),
        PluginStateStoreErrorKind::NotFound | PluginStateStoreErrorKind::PermissionDenied => {
            (ErrorCode::PermissionDenied, "state owner is not authorized")
        }
        PluginStateStoreErrorKind::Unavailable => {
            (ErrorCode::Upstream, "state store is unavailable")
        }
    };
    PluginFault::new(code, message)
}

fn denied_fault() -> PluginFault {
    PluginFault::new(
        ErrorCode::PermissionDenied,
        "state namespace is not authorized",
    )
}

fn invalid_fault() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "state input is invalid")
}

fn internal_fault() -> PluginFault {
    PluginFault::new(ErrorCode::Fault, "state response could not be encoded")
}
