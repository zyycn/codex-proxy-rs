//! 执行扩展索引的代次隔离、计划保活与租约替身

use std::sync::Arc;

use gateway_core::engine::extensions::{ExecutionExtensionIndex, ExecutionExtensionPlans};
use gateway_core::engine::upstream_adapter::{UpstreamAdapterPlan, UpstreamAdapterSelection};
use gateway_core::routing::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference};
use gateway_core::{
    engine::AttemptContext, error::ProviderError, identity::ProviderKind, routing::UpstreamModelId,
};

struct ReadyLease;

impl ExtensionSetLease for ReadyLease {
    fn is_ready(&self) -> bool {
        true
    }
}

pub(super) fn reference(id: &str) -> ExtensionSetReference {
    let id = ExtensionSetId::new(id.into()).unwrap();
    ExtensionSetReference::new(id, Arc::new(ReadyLease))
}

#[derive(Debug)]
struct EmptyAdapterPlan;

impl UpstreamAdapterPlan for EmptyAdapterPlan {
    fn select(
        &self,
        _: gateway_core::engine::ModelRouteScope<'_>,
        _: &ProviderKind,
        _: Option<&gateway_core::account::ProviderAccountId>,
    ) -> Result<UpstreamAdapterSelection, ProviderError> {
        Ok(UpstreamAdapterSelection::Native)
    }
}

struct PublishedPlans(Arc<ExecutionExtensionPlans>);

impl ExtensionSetLease for PublishedPlans {
    fn is_ready(&self) -> bool {
        self.0.upstream_adapters.is_some()
    }
}

#[test]
fn adapter_only_generation_stays_alive_until_the_last_frozen_plan_is_released() {
    let index = ExecutionExtensionIndex::default();
    let id = ExtensionSetId::new("adapter-only".into()).unwrap();
    let plans = Arc::new(ExecutionExtensionPlans {
        middleware: None,
        upstream_adapters: Some(Arc::new(EmptyAdapterPlan)),
    });
    let weak_plans = Arc::downgrade(&plans);
    let published = Arc::new(PublishedPlans(index.register(id.clone(), plans).unwrap()));
    let weak_publication = Arc::downgrade(&published);
    let reference = ExtensionSetReference::new(id.clone(), published);
    assert!(index.middleware(&reference).is_none());
    let frozen = index.upstream_adapters(&reference).unwrap();
    drop(reference);

    // 当前发布可以退场；在途计划仍保活整个代次，不能覆盖相同身份
    assert!(weak_publication.upgrade().is_some());
    assert!(weak_plans.upgrade().is_some());
    assert!(index.register(id.clone(), Arc::default()).is_err());
    let in_flight = frozen.clone();
    drop(frozen);
    assert!(weak_publication.upgrade().is_some());
    drop(in_flight);

    assert!(weak_publication.upgrade().is_none());
    assert!(weak_plans.upgrade().is_none());
    let released = ExtensionSetReference::new(id.clone(), Arc::new(ReadyLease));
    assert!(index.upstream_adapters(&released).is_none());
    let _replacement = index.register(id, Arc::default()).unwrap();
}

#[test]
fn execution_index_does_not_own_registered_generations() {
    let index = ExecutionExtensionIndex::default();
    let reference = reference("unpublished");
    let plans = Arc::new(ExecutionExtensionPlans::default());
    let weak = Arc::downgrade(&plans);
    let owner = index.register(reference.id().clone(), plans).unwrap();
    assert!(weak.upgrade().is_some());
    drop(owner);
    assert!(weak.upgrade().is_none());
    assert!(index.middleware(&reference).is_none());
    assert!(index.upstream_adapters(&reference).is_none());
}

#[derive(Debug, Default)]
struct AccountSelectionProbe(std::sync::Mutex<Vec<Option<String>>>);

impl UpstreamAdapterPlan for AccountSelectionProbe {
    fn select(
        &self,
        _: gateway_core::engine::ModelRouteScope<'_>,
        _: &ProviderKind,
        account: Option<&gateway_core::account::ProviderAccountId>,
    ) -> Result<UpstreamAdapterSelection, ProviderError> {
        self.0
            .lock()
            .unwrap()
            .push(account.map(|account| account.as_str().into()));
        Ok(UpstreamAdapterSelection::Native)
    }
}

#[test]
fn adapter_preselection_follows_only_the_current_attempt_account_owner() {
    use gateway_core::{
        account::{AccountSelectionPolicy, ProviderAccountId, RotationStrategy},
        engine::continuation::{ContinuationBinding, NativeContinuationPin, PreviousResponseId},
        engine::upstream_adapter::FrozenUpstreamAdapterPlan,
        engine::{
            AccountAttemptContext, ContinuationAttempt, ModelRequestId, RequestAttemptContext,
        },
        lifecycle::CancellationToken,
        policy::ClientApiKeyId,
    };
    use std::{
        num::NonZeroU32,
        time::{Duration, SystemTime},
    };

    let provider = ProviderKind::new("openai").unwrap();
    let model = UpstreamModelId::new("model").unwrap();
    let key = ClientApiKeyId::new("key").unwrap();
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("public"),
        PreviousResponseId::new("upstream"),
        key.clone(),
        provider.clone(),
        ProviderAccountId::new("acct_pinned").unwrap(),
    );
    for (attempt, required, expected) in [
        (ContinuationAttempt::Native, None, Some("acct_pinned")),
        (ContinuationAttempt::ReplayAny, None, None),
        (ContinuationAttempt::None, None, None),
        (
            ContinuationAttempt::Native,
            Some("acct_required"),
            Some("acct_required"),
        ),
    ] {
        let probe = Arc::new(AccountSelectionProbe::default());
        let context = AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new("req_account_plan").unwrap(),
                key.clone(),
            )
            .with_upstream_adapters(Some(FrozenUpstreamAdapterPlan::new(
                probe.clone(),
                reference("account-plan"),
            ))),
            NonZeroU32::MIN,
            SystemTime::now() + Duration::from_secs(5),
            AccountSelectionPolicy::new(RotationStrategy::RoundRobin, NonZeroU32::MIN),
            AccountAttemptContext::new(
                Default::default(),
                required.map(|id| ProviderAccountId::new(id).unwrap()),
                None,
            ),
            Some(ContinuationBinding::Pinned(pin.clone())),
            CancellationToken::new(),
        )
        .with_continuation_attempt(attempt);
        assert!(matches!(
            context.upstream_adapter(&provider, &model).unwrap(),
            UpstreamAdapterSelection::Native
        ));
        assert_eq!(*probe.0.lock().unwrap(), vec![expected.map(str::to_owned)]);
    }
}
