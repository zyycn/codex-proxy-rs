//! 单次上游尝试的通用保护反馈；副作用有界等待且不改变原始执行结果

use std::{sync::Arc, time::Duration};

use futures::{FutureExt, future::BoxFuture, pin_mut, select_biased};
use futures_timer::Delay;

use super::provider::ProviderCallMetadata;
use crate::{
    account::{AccountErrorFacts, CompiledAccountErrorProtection},
    error::ProviderError,
    event::ProviderEvent,
    provider_ports::{AccountProtectionGuard, ProviderCooldownPort},
    routing::ConfigRevision,
};

const FEEDBACK_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub(super) struct ProtectionContext {
    pub(super) store: Arc<dyn ProviderCooldownPort>,
    pub(super) policy: Arc<CompiledAccountErrorProtection>,
    pub(super) revision: u64,
}

impl std::fmt::Debug for ProtectionContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProtectionContext")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

pub(super) struct AttemptProtection {
    store: Arc<dyn ProviderCooldownPort>,
    guard: AccountProtectionGuard,
    policy: Arc<CompiledAccountErrorProtection>,
    provider: crate::identity::ProviderKind,
    completed: bool,
}

impl AttemptProtection {
    pub(super) async fn begin(
        store: Arc<dyn ProviderCooldownPort>,
        policy: Arc<CompiledAccountErrorProtection>,
        policy_revision: u64,
        metadata: &ProviderCallMetadata,
    ) -> Option<Self> {
        if !policy.policy().enabled {
            return None;
        }
        let revision = metadata.credential_revision()?;
        let policy_revision = ConfigRevision::new(policy_revision).ok()?;
        let guard = match bounded(store.begin_protection(
            metadata.provider_account_id(),
            revision,
            policy.fingerprint(),
            policy_revision,
        ))
        .await
        {
            Ok(Ok(guard)) => guard?,
            _ => {
                tracing::warn!("account protection feedback could not start");
                return None;
            }
        };
        Some(Self {
            store,
            guard,
            policy,
            provider: metadata.provider().clone(),
            completed: false,
        })
    }

    pub(super) fn observe(&mut self, event: &ProviderEvent) {
        self.completed |= event
            .canonical_facts()
            .iter()
            .any(|fact| matches!(fact, crate::event::GatewayEvent::Completed(_)));
    }

    pub(super) const fn is_completed(&self) -> bool {
        self.completed
    }

    pub(super) fn complete(
        self,
        result: Option<Result<ProviderEvent, ProviderError>>,
    ) -> BoxFuture<'static, Option<Result<ProviderEvent, ProviderError>>> {
        Box::pin(async move {
            let feedback = match &result {
                Some(Err(error)) => {
                    let Some(fields) = error.upstream_failure_fields() else {
                        return result;
                    };
                    let matched = self.policy.matching_rules(AccountErrorFacts {
                        provider: self.provider.as_str(),
                        status: fields.status,
                        code: fields.code.as_deref(),
                        error_type: fields.error_type.as_deref(),
                        message: fields.message.as_deref(),
                    });
                    if matched.is_empty() {
                        return result;
                    }
                    bounded(self.store.record_protection_failure(
                        &self.guard,
                        self.policy.policy(),
                        &matched,
                    ))
                    .await
                }
                None | Some(Ok(_)) if self.completed => {
                    bounded(self.store.record_protection_success(&self.guard)).await
                }
                _ => return result,
            };
            if !matches!(feedback, Ok(Ok(_))) {
                tracing::warn!("account protection feedback was not persisted");
            }
            result
        })
    }
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> Result<T, ()> {
    let future = future.fuse();
    let timeout = Delay::new(FEEDBACK_TIMEOUT).fuse();
    pin_mut!(future, timeout);
    select_biased! {
        value = future => Ok(value),
        _ = timeout => Err(()),
    }
}
