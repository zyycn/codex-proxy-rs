//! 验证 xAI 真实选择器将插件过滤传递到租约重试、等待与原生续写

use super::*;
use futures::FutureExt;
use gateway_core::account::ProviderAccountId;
use gateway_core::concurrency::ConcurrencyQueuePolicy;
use gateway_core::engine::policy::{
    AccountScheduleDecision, AccountScheduleInput, ModelRouteDecision, ModelRouteInput,
    RequestPolicyContext, RequestPolicyFault, RequestPolicyPlan,
};

#[derive(Debug)]
struct Filter {
    all: bool,
    seen: Arc<Mutex<Vec<AccountScheduleInput>>>,
}

impl RequestPolicyPlan for Filter {
    fn route_model(
        &self,
        _: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>> {
        Box::pin(async { Ok(ModelRouteDecision::Unhandled) })
    }

    fn schedule_account(
        &self,
        input: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>> {
        let excluded = input
            .candidates()
            .iter()
            .filter(|candidate| self.all || candidate.account_id() == &account_id("filter-a"))
            .map(|candidate| candidate.account_id().clone())
            .collect();
        self.seen.lock().unwrap().push(input);
        Box::pin(async move { Ok(AccountScheduleDecision::DelegateFiltered(excluded)) })
    }
}

#[derive(Default)]
struct Leases {
    busy: Mutex<bool>,
    deny_second: bool,
    acquired: Mutex<Vec<ProviderAccountId>>,
}

impl ProviderLeasePort for Leases {
    fn load_state<'a>(
        &'a self,
        key: &'a ClientApiKeyId,
        provider: &'a ProviderKind,
        ids: &'a [ProviderAccountId],
        pool: gateway_core::provider_ports::ProviderConcurrencyPool,
    ) -> BoxFuture<'a, Result<ProviderSchedulingState, ProviderStoreError>> {
        Box::pin(async move {
            let state = DiagnosticLeasePort
                .load_state(key, provider, ids, pool)
                .await?;
            let mut signals = state.signals().clone();
            if *self.busy.lock().unwrap() {
                for signal in signals.values_mut() {
                    signal.in_flight = u32::MAX;
                }
            }
            Ok(ProviderSchedulingState::new(signals, 0))
        })
    }

    fn try_acquire(
        &self,
        request: ProviderLeaseRequest,
    ) -> BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
        Box::pin(async move {
            let ProviderLeaseRequest::Scheduling(request) = request else {
                panic!("scheduling lease")
            };
            self.acquired
                .lock()
                .unwrap()
                .push(request.account_id().clone());
            if self.deny_second && request.account_id() == &account_id("filter-b") {
                Ok(ProviderLeaseAcquisition::Busy {
                    retry_after: Some(Duration::from_millis(10)),
                })
            } else {
                Ok(ProviderLeaseAcquisition::Acquired(Box::new(())))
            }
        })
    }
}

#[tokio::test]
async fn filtered_scheduling_covers_lease_retry_waiting_and_pinned_terminal_rejection() {
    for mode in ["retry", "wait", "all", "pin"] {
        let store = MemoryProviderAccountStore::shared();
        let repository = GrokCredentialRepository::new(store.clone());
        let mut accounts = BTreeMap::new();
        let kind = ProviderKind::new("xai").unwrap();
        for suffix in ["filter-a", "filter-b", "filter-c"] {
            let input = create_input(suffix, suffix);
            seed_input(&store, &input).await.unwrap();
            repository
                .update_state(&UpdateGrokCredentialState {
                    account_id: input.account_id.clone(),
                    expected_revision: CredentialRevision::new(1).unwrap(),
                    credential_state: CredentialState::Ready,
                    error_reason: None,
                    error_message: None,
                    observed_at: chrono::Utc::now(),
                })
                .await
                .unwrap();
            accounts.insert(
                input.account_id,
                RuntimeAccount::new(kind.clone(), BTreeSet::new()),
            );
        }
        let leases = Arc::new(Leases {
            busy: Mutex::new(matches!(mode, "wait" | "all")),
            deny_second: mode == "retry",
            ..Default::default()
        });
        let cache = MemoryGrokCatalogCache::shared();
        let selector = GrokAccountSessionSelector::new(
            kind.clone(),
            repository.clone(),
            cache.clone(),
            Arc::new(crate::support::grok_quota_service(
                repository.clone(),
                Arc::new(UnavailableBillingTransport),
            )),
            leases.clone(),
            Arc::new(MemoryCooldownPort::default()),
            Arc::new(AccountFeedbackStats::default()),
        );
        let transport = StubInferenceTransport::success();
        let provider = Arc::new(
            GrokBuildProvider::new(
                Arc::new(selector),
                transport.clone(),
                Arc::new(crate::support::grok_catalog_service(
                    repository,
                    Arc::new(StaticCatalogTransport),
                    cache,
                )),
                StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
                Arc::new(AccountFeedbackStats::default()),
                crate::support::xai_wire_profile(),
            )
            .unwrap(),
        );
        let id = ModelRequestId::new("req_xai_filter").unwrap();
        let key = ClientApiKeyId::new("key_xai_contract").unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let policy = RequestPolicyContext::new(
            Arc::new(Filter {
                all: mode == "all",
                seen: seen.clone(),
            }),
            ExtensionSetReference::new(
                ExtensionSetId::new("filter-test".into()).unwrap(),
                Arc::new(TestExtensionLease),
            ),
            id.clone(),
            key.clone(),
            vec![],
        );
        let continuation = (mode == "pin").then(|| {
            ContinuationBinding::Pinned(NativeContinuationPin::new(
                PreviousResponseId::new("client_response"),
                PreviousResponseId::new("upstream_response"),
                key.clone(),
                kind,
                account_id("filter-a"),
            ))
        });
        let scope = Arc::new(FrozenAccountScope::new(
            Arc::new(RuntimeAccountDirectory::new(accounts)),
            ClientRoutingScope::all_accounts(),
        ));
        let operation = operation();
        let kind = ProviderKind::new("xai").unwrap();
        let snapshot = RuntimeSnapshot::new(
            ConfigRevision::new(1).unwrap(),
            gateway_core::settings::SettingsValues::new(2, "smart", Default::default(), None, None),
            vec![kind.clone()],
            vec![ProviderModel::new(
                kind,
                UpstreamModelId::new(MODEL).unwrap(),
                ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(131_072))
                    .with_feature(Feature::NativeContinuation, SupportLevel::Native),
            )],
            vec![],
        )
        .unwrap();
        let plan = snapshot
            .plan(
                &PublicModelId::new(MODEL).unwrap(),
                &operation,
                scope.clone(),
                &RoutingContext::default(),
            )
            .unwrap();
        let request = ProviderRequest::new(operation, plan.candidates()[0].clone());
        let attempt = AttemptContext::new(
            gateway_core::engine::RequestAttemptContext::new(id, key)
                .with_request_policy(Some(policy)),
            NonZeroU32::MIN,
            SystemTime::now() + Duration::from_secs(5),
            selection_policy().with_queue(ConcurrencyQueuePolicy {
                max_waiting: 1,
                timeout: Duration::from_secs(2),
            }),
            AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
            continuation,
            CancellationToken::new(),
        );
        let mut pending = Box::pin(provider.execute(request, attempt));
        if mode == "wait" {
            assert!(pending.as_mut().now_or_never().is_none());
            assert!(leases.acquired.lock().unwrap().is_empty());
            *leases.busy.lock().unwrap() = false;
        }
        let result = pending.await;
        if matches!(mode, "all" | "pin") {
            let error = result.err().expect("terminal filter rejection");
            assert_eq!(
                error.kind(),
                ProviderErrorKind::RequestPolicyDenied,
                "{mode}"
            );
            assert_eq!(error.send_state(), UpstreamSendState::NotSent);
            assert!(leases.acquired.lock().unwrap().is_empty());
            assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
            if mode == "pin" {
                assert_eq!(seen.lock().unwrap()[0].candidates().len(), 1);
            }
        } else {
            let stream = result.expect("allowed account");
            assert_ne!(
                stream.metadata().provider_account_id(),
                &account_id("filter-a")
            );
            assert!(
                leases
                    .acquired
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|id| id != &account_id("filter-a"))
            );
            if mode == "retry" {
                assert_eq!(
                    stream.metadata().provider_account_id(),
                    &account_id("filter-c")
                );
                assert!(
                    seen.lock()
                        .unwrap()
                        .iter()
                        .flat_map(|input| input.candidates())
                        .any(
                            |candidate| candidate.account_id() == &account_id("filter-b")
                                && !candidate.available()
                        )
                );
            }
        }
        assert!(store.account(&account_id("filter-a")).unwrap().enabled());
    }
}
