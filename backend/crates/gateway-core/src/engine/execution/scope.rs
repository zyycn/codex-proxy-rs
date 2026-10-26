//! 请求嵌套图、活动权限与作用域 guard

use super::admission::{AdmissionLease, settle_budget};
use super::contract::{AuthenticatedClient, ExecutionRequestMetadata};
use super::{MAX_CONCURRENT_NESTED_EXECUTIONS, MAX_NESTED_EXECUTIONS};
use crate::engine::budget::{ClientBudgetCharge, ClientBudgetPort};
use crate::{
    concurrency::ConcurrencyWaitBudget,
    engine::{
        ModelRequestId, ProviderAccountId, coordinator::CoordinationExtensions,
        extensions::ExtensionCallScope, nested::ExecutionEffects,
    },
    error::{GatewayError, GatewayErrorKind},
    identity::ProviderKind,
    lifecycle::{CancellationToken, Deadline},
    operation::Operation,
    policy::ClientApiKeyId,
    routing::{FrozenAccountScope, PublicModelId, UpstreamModelId},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};

pub(super) enum ExecutionTarget {
    Model(PublicModelId),
    ProviderEndpoint {
        provider: ProviderKind,
        upstream_model: Option<UpstreamModelId>,
    },
}

impl ExecutionTarget {
    pub(super) fn public_model(&self) -> Option<&PublicModelId> {
        match self {
            Self::Model(model) => Some(model),
            Self::ProviderEndpoint { .. } => None,
        }
    }

    pub(super) fn into_public_model(self) -> Option<PublicModelId> {
        match self {
            Self::Model(model) => Some(model),
            Self::ProviderEndpoint { .. } => None,
        }
    }
}

pub(super) struct PendingStartExecution {
    pub(super) client: AuthenticatedClient,
    pub(super) target: ExecutionTarget,
    pub(super) operation: Operation,
    pub(super) metadata: ExecutionRequestMetadata,
}

pub(super) struct AuthorizedExecution {
    pub(super) response_control: Option<crate::engine::response_control::ResponseControl>,
    pub(super) account_scope: Arc<FrozenAccountScope>,
    pub(super) deadline_at: Deadline,
    pub(super) cancellation: CancellationToken,
    pub(super) extension_scope: ExtensionCallScope,
    pub(super) required_provider: Option<ProviderKind>,
    pub(super) required_account: Option<ProviderAccountId>,
    pub(super) nested: Option<NestedExecutionFacts>,
    pub(super) bound: Option<BoundExecutionFacts>,
    pub(super) graph: Option<Arc<NestedExecutionGraph>>,
    pub(super) execution_effects_baseline: usize,
    pub(super) nested_permit: Option<NestedExecutionPermit>,
}

pub(super) struct ExecutionStartGuard {
    pub(super) admission: ExecutionAdmission,
    pub(super) active_request: Option<ActiveRequestLease>,
    pub(super) concurrency_wait_budget: ConcurrencyWaitBudget,
    pub(super) admission_decision_ms: Option<u64>,
}

pub(super) struct PreparedExecutionStart {
    pub(super) request_id: ModelRequestId,
    pub(super) started_at: SystemTime,
    pub(super) plan: crate::routing::RoutingPlan,
    pub(super) extensions: CoordinationExtensions,
    pub(super) authorization: AuthorizedExecution,
    pub(super) guard: ExecutionStartGuard,
}

pub(super) struct NestedExecutionFacts {
    pub(super) parent_request_id: ModelRequestId,
    pub(super) initiating_plugin_instance_id: String,
}

pub(super) struct BoundExecutionFacts {
    pub(super) initiating_plugin_instance_id: String,
}

pub(super) struct NestedExecutionGraph {
    pub(super) total_started: AtomicUsize,
    pub(super) active: AtomicUsize,
    pub(super) effects: Arc<ExecutionEffects>,
}

impl NestedExecutionGraph {
    pub(super) fn new(effects: Arc<ExecutionEffects>) -> Self {
        Self {
            total_started: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            effects,
        }
    }

    pub(super) fn acquire(self: &Arc<Self>) -> Result<NestedExecutionPermit, GatewayError> {
        if self
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CONCURRENT_NESTED_EXECUTIONS).then_some(active + 1)
            })
            .is_err()
        {
            return Err(GatewayError::new(
                GatewayErrorKind::ConcurrencyQueueFull,
                "nested model execution concurrency is exhausted",
            ));
        }
        if self
            .total_started
            .try_update(Ordering::AcqRel, Ordering::Acquire, |total| {
                (total < MAX_NESTED_EXECUTIONS).then_some(total + 1)
            })
            .is_err()
        {
            self.active.fetch_sub(1, Ordering::AcqRel);
            return Err(GatewayError::new(
                GatewayErrorKind::PolicyDenied,
                "nested model execution budget is exhausted",
            ));
        }
        Ok(NestedExecutionPermit {
            graph: Arc::clone(self),
            armed: true,
        })
    }
}

pub(super) struct NestedExecutionPermit {
    pub(super) graph: Arc<NestedExecutionGraph>,
    pub(super) armed: bool,
}

impl NestedExecutionPermit {
    pub(super) fn release(mut self) {
        if self.armed {
            self.graph.active.fetch_sub(1, Ordering::AcqRel);
            self.armed = false;
        }
    }
}

impl Drop for NestedExecutionPermit {
    fn drop(&mut self) {
        if self.armed {
            self.graph.active.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

pub(super) struct ActiveRequestAuthority {
    pub(super) client: AuthenticatedClient,
    pub(super) account_scope: Arc<FrozenAccountScope>,
    pub(super) deadline_at: Deadline,
    pub(super) cancellation: CancellationToken,
    pub(super) extension_scope: ExtensionCallScope,
    pub(super) graph: Arc<NestedExecutionGraph>,
}

pub(super) type ActiveRequestRegistry =
    Arc<Mutex<BTreeMap<ModelRequestId, Weak<ActiveRequestAuthority>>>>;

pub(super) struct ActiveRequestLease {
    pub(super) registry: ActiveRequestRegistry,
    pub(super) request_id: ModelRequestId,
    pub(super) authority: Arc<ActiveRequestAuthority>,
}

impl ActiveRequestLease {
    pub(super) fn release(self) {
        drop(self);
    }
}

impl Drop for ActiveRequestLease {
    fn drop(&mut self) {
        self.authority.cancellation.cancel();
        let mut active = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active
            .get(&self.request_id)
            .and_then(Weak::upgrade)
            .is_some_and(|authority| Arc::ptr_eq(&authority, &self.authority))
        {
            active.remove(&self.request_id);
        }
    }
}

impl ExecutionStartGuard {
    pub(super) async fn release_failed(
        self,
        budget: Option<&dyn ClientBudgetPort>,
        request_id: ModelRequestId,
        key_id: ClientApiKeyId,
        diagnostics: &dyn crate::diagnostics::OperationalDiagnostics,
    ) {
        if let Some(active_request) = self.active_request {
            active_request.release();
        }
        if let Some(budget) = budget {
            settle_budget(
                budget,
                ClientBudgetCharge {
                    key_id,
                    request_id,
                    amount_usd: crate::metering::Decimal::ZERO,
                    completed_at: SystemTime::now(),
                },
                diagnostics,
            )
            .await;
        }
        self.admission.release().await;
    }
}

pub(super) enum ExecutionAdmission {
    Client(AdmissionLease),
    Nested(NestedExecutionPermit),
    Bound(AdmissionLease, NestedExecutionPermit),
}

impl ExecutionAdmission {
    pub(super) async fn release(self) {
        match self {
            Self::Client(admission) => admission.release().await,
            Self::Nested(permit) => permit.release(),
            Self::Bound(admission, permit) => {
                admission.release().await;
                permit.release();
            }
        }
    }
}
