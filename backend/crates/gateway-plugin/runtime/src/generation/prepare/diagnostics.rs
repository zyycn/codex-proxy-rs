//! 将已发布与准备中的实例事实投影为运行诊断，不参与候选提交

use super::super::restart_circuit::RestartIdentity;
use super::{
    PluginRuntime,
    set::{PreparedSet, instance_fingerprint},
};
use async_trait::async_trait;
use gateway_admin::model::{
    AdminError, AdminErrorKind, Revision,
    plugins::instances::{
        PluginInstance, PluginInstanceRuntime, PluginInstanceRuntimeFailure,
        PluginInstanceRuntimeLoad, PluginInstanceRuntimeStatus, PluginInstanceSnapshot,
    },
};
use gateway_core::routing::extensions::ExtensionSetReference;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
};

const MAXIMUM_PREPARATION_DIAGNOSTICS: usize = 8;
#[derive(Clone)]
pub(super) enum PreparationDiagnostic {
    Preparing,
    Prepared { set_id: String },
    Failed(PluginInstanceRuntimeFailure),
}
impl PluginRuntime {
    pub(super) fn record_preparation(&self, revision: u64, diagnostic: PreparationDiagnostic) {
        let mut diagnostics = self
            .preparation_diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        diagnostics.insert(revision, diagnostic);
        while diagnostics.len() > MAXIMUM_PREPARATION_DIAGNOSTICS {
            diagnostics.pop_first();
        }
    }

    fn preparation_diagnostic(&self, revision: u64) -> Option<PreparationDiagnostic> {
        self.preparation_diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&revision)
            .cloned()
    }
}
#[async_trait]
impl gateway_admin::ports::plugins::PluginRuntimeDiagnostics for PluginRuntime {
    async fn runtime_diagnostics(
        &self,
        snapshot: &PluginInstanceSnapshot,
        published_revision: Option<u64>,
        published: Option<&ExtensionSetReference>,
    ) -> Option<BTreeMap<String, PluginInstanceRuntime>> {
        let sets = {
            let mut prepared = self.prepared.lock().await;
            prepared.retain(|_, set| set.strong_count() > 0);
            prepared
                .values()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        let open_restart_circuits = snapshot
            .instances
            .iter()
            .filter(|instance| instance.enabled)
            .filter_map(|instance| {
                let identity =
                    RestartIdentity::new(instance.id.clone(), instance_fingerprint(instance).ok()?);
                self.restart_circuit_is_open(&identity)
                    .then(|| instance.id.clone())
            })
            .collect::<BTreeSet<_>>();
        let published_set = published
            .and_then(|reference| sets.iter().find(|set| set.id == *reference.id()).cloned());
        let published_ready = published.is_some_and(ExtensionSetReference::can_serve);
        let preparation = self.preparation_diagnostic(snapshot.config_revision.get());
        let projection = InstanceRuntimeProjection {
            target_revision: snapshot.config_revision,
            published_revision,
            published_ready,
            published_set: published_set.as_deref(),
            sets: &sets,
            preparation: preparation.as_ref(),
            open_restart_circuits: &open_restart_circuits,
        };
        Some(
            snapshot
                .instances
                .iter()
                .map(|instance| {
                    let runtime = projection.project(instance);
                    (instance.id.clone(), runtime)
                })
                .collect(),
        )
    }
}
struct InstanceRuntimeProjection<'a> {
    target_revision: Revision,
    published_revision: Option<u64>,
    published_ready: bool,
    published_set: Option<&'a PreparedSet>,
    sets: &'a [Arc<PreparedSet>],
    preparation: Option<&'a PreparationDiagnostic>,
    open_restart_circuits: &'a BTreeSet<String>,
}

impl InstanceRuntimeProjection<'_> {
    fn project(&self, expected: &PluginInstance) -> PluginInstanceRuntime {
        let active = self.published_set.and_then(|set| {
            set.sessions
                .iter()
                .find(|instance| instance.instance_id == expected.id)
        });
        let actual_matches = active.is_some_and(|instance| {
            instance.revision == expected.revision
                && instance.artifact_sha256 == expected.artifact_sha256
        });
        let target_is_published = self.published_revision == Some(self.target_revision.get());
        let candidate_set = match self.preparation {
            Some(PreparationDiagnostic::Prepared { set_id }) => Some(set_id.as_str()),
            _ => None,
        };
        let published_set_id = self.published_set.map(|set| set.id.as_str());
        let mut draining_revisions = BTreeSet::new();
        for set in self.sets {
            if Some(set.id.as_str()) == published_set_id || Some(set.id.as_str()) == candidate_set {
                continue;
            }
            if let Some(instance) = set
                .sessions
                .iter()
                .find(|instance| instance.instance_id == expected.id)
                && instance.revision.get() < expected.revision.get()
            {
                draining_revisions.insert(instance.revision.get());
            }
        }
        if !expected.enabled
            && let Some(instance) = active
            && instance.revision != expected.revision
        {
            draining_revisions.insert(instance.revision.get());
        }

        let diagnostic = active.map(|instance| instance.session.diagnostic());
        let mut failure = None;
        let status = if !expected.enabled {
            if active.is_some() || !draining_revisions.is_empty() {
                PluginInstanceRuntimeStatus::Draining
            } else {
                PluginInstanceRuntimeStatus::Disabled
            }
        } else if target_is_published
            && let Some(reason) = self
                .published_set
                .and_then(|set| set.failures.get(&expected.id))
        {
            failure = Some(reason.clone());
            PluginInstanceRuntimeStatus::PreparationFailed
        } else if self.open_restart_circuits.contains(&expected.id) {
            let message = match diagnostic {
                Some(crate::rpc::RpcSessionDiagnostic::Failed { message, .. }) => {
                    format!("{message}，已暂停自动重启")
                }
                _ => "插件实例连续异常退出，已暂停自动重启".to_owned(),
            };
            failure = Some(runtime_failure("restart_circuit_open", &message));
            PluginInstanceRuntimeStatus::Faulted
        } else if actual_matches
            && let Some(crate::rpc::RpcSessionDiagnostic::Failed { code, message }) = diagnostic
        {
            failure = Some(runtime_failure(code, message));
            PluginInstanceRuntimeStatus::Faulted
        } else if !target_is_published {
            match self.preparation {
                Some(PreparationDiagnostic::Preparing) => PluginInstanceRuntimeStatus::Preparing,
                Some(PreparationDiagnostic::Failed(reason)) => {
                    failure = Some(reason.clone());
                    PluginInstanceRuntimeStatus::PreparationFailed
                }
                Some(PreparationDiagnostic::Prepared { .. }) | None => {
                    PluginInstanceRuntimeStatus::AwaitingPublication
                }
            }
        } else if !actual_matches {
            failure = Some(runtime_failure(
                "published_instance_mismatch",
                "已发布插件实例与期望版本不一致",
            ));
            PluginInstanceRuntimeStatus::Blocked
        } else {
            match diagnostic {
                Some(crate::rpc::RpcSessionDiagnostic::Ready) if self.published_ready => {
                    PluginInstanceRuntimeStatus::Running
                }
                Some(crate::rpc::RpcSessionDiagnostic::Ready) => {
                    failure = Some(runtime_failure(
                        "published_set_unready",
                        "同一发布集合中存在未就绪插件实例",
                    ));
                    PluginInstanceRuntimeStatus::Blocked
                }
                Some(crate::rpc::RpcSessionDiagnostic::Quiescing) => {
                    PluginInstanceRuntimeStatus::Draining
                }
                Some(crate::rpc::RpcSessionDiagnostic::Failed { code, message }) => {
                    failure = Some(runtime_failure(code, message));
                    PluginInstanceRuntimeStatus::Faulted
                }
                None => {
                    failure = Some(runtime_failure(
                        "instance_not_published",
                        "已发布集合缺少该插件实例",
                    ));
                    PluginInstanceRuntimeStatus::Blocked
                }
            }
        };

        PluginInstanceRuntime {
            status,
            actual_revision: active.map(|instance| instance.revision.get()),
            actual_artifact_sha256: active.map(|instance| instance.artifact_sha256.clone()),
            failure,
            draining_revisions: draining_revisions.into_iter().collect(),
            load: active.map(|instance| {
                let load = instance.session.load();
                PluginInstanceRuntimeLoad {
                    active_calls: load.active_calls,
                    executing_calls: load.executing_calls,
                    queued_calls: load.queued_calls,
                    resuming_calls: load.resuming_calls,
                    pending_callbacks: load.pending_callbacks,
                    active_streams: load.active_streams,
                }
            }),
        }
    }
}

fn runtime_failure(code: &str, message: &str) -> PluginInstanceRuntimeFailure {
    PluginInstanceRuntimeFailure {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

pub(super) fn runtime_failure_from_admin(error: &AdminError) -> PluginInstanceRuntimeFailure {
    let code = match error.kind() {
        AdminErrorKind::Invalid => "invalid",
        AdminErrorKind::Unauthorized => "unauthorized",
        AdminErrorKind::Forbidden => "forbidden",
        AdminErrorKind::NotFound => "not_found",
        AdminErrorKind::Conflict => "conflict",
        AdminErrorKind::RateLimited => "rate_limited",
        AdminErrorKind::BadGateway => "bad_gateway",
        AdminErrorKind::UpstreamResultUnknown => "upstream_result_unknown",
        AdminErrorKind::Unavailable => "unavailable",
        AdminErrorKind::Internal => "internal",
    };
    runtime_failure(code, error.message())
}
