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

#[derive(Debug)]
struct PreferAdapterAccount(ProviderAccountId);

impl RequestPolicyPlan for PreferAdapterAccount {
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
        let preferred = self.0.clone();
        let decision = if input
            .candidates()
            .iter()
            .any(|candidate| candidate.account_id() == &preferred)
        {
            AccountScheduleDecision::Pick(preferred)
        } else {
            AccountScheduleDecision::Delegate
        };
        Box::pin(async move { Ok(decision) })
    }
}

#[derive(Debug, Clone, Copy)]
enum AdapterState {
    Available,
    Unavailable,
    AuthenticationMismatch,
}

#[derive(Debug)]
struct AccountEligibilityPlan {
    state: AdapterState,
    polls: Arc<std::sync::atomic::AtomicUsize>,
}

impl gateway_core::engine::upstream_adapter::UpstreamAdapterPlan for AccountEligibilityPlan {
    fn select(
        &self,
        _: &AttemptContext,
        _: &ProviderKind,
        _: &UpstreamModelId,
        account: Option<&ProviderAccountId>,
    ) -> Result<UpstreamAdapterSelection, ProviderError> {
        let Some(account) = account else {
            return Ok(UpstreamAdapterSelection::PendingAccount);
        };
        if account != &account_id("adapter-filter-a") {
            return Ok(UpstreamAdapterSelection::Native);
        }
        match self.state {
            AdapterState::Unavailable => Err(ProviderError::new(
                ProviderErrorKind::Unavailable,
                UpstreamSendState::NotSent,
            )),
            AdapterState::Available | AdapterState::AuthenticationMismatch => Ok(
                UpstreamAdapterSelection::Adapter(Arc::new(AccountAdapter {
                    supports_authentication: matches!(self.state, AdapterState::Available),
                    polls: Arc::clone(&self.polls),
                })),
            ),
        }
    }
}

#[derive(Debug)]
struct AccountAdapter {
    supports_authentication: bool,
    polls: Arc<std::sync::atomic::AtomicUsize>,
}

impl gateway_core::engine::upstream_adapter::UpstreamAdapter for AccountAdapter {
    fn transport(&self) -> &str {
        "http_sse"
    }

    fn supports_authentication(&self, _: &str) -> bool {
        self.supports_authentication
    }

    fn execute(
        self: Arc<Self>,
        _: gateway_core::engine::upstream_adapter::UpstreamAdapterInvocation,
    ) -> gateway_core::engine::provider::EventStream {
        Box::pin(futures::stream::once(async move {
            self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(ProviderError::new(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ))
        }))
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
                let seen = seen.lock().unwrap();
                let [owner] = seen[0].candidates() else {
                    panic!("native continuation must have one visible owner");
                };
                assert_eq!(owner.account_id(), &account_id("filter-a"));
                assert_eq!(
                    owner.role(),
                    gateway_core::engine::policy::AccountScheduleRole::Pinned
                );
                assert!(!owner.session_bound());
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

#[tokio::test]
async fn account_scoped_adapter_filters_candidates_by_adapter_and_native_request_eligibility() {
    for (state, prewarm) in [
        (AdapterState::Available, false),
        (AdapterState::Unavailable, false),
        (AdapterState::AuthenticationMismatch, false),
        (AdapterState::Available, true),
        (AdapterState::Unavailable, true),
        (AdapterState::AuthenticationMismatch, true),
    ] {
        let store = MemoryProviderAccountStore::shared();
        let repository = GrokCredentialRepository::new(store.clone());
        let mut accounts = BTreeMap::new();
        let kind = ProviderKind::new("xai").unwrap();
        for suffix in ["adapter-filter-a", "adapter-filter-b"] {
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
        let leases = Arc::new(Leases::default());
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
        let provider = Arc::new(
            GrokBuildProvider::new(
                Arc::new(selector),
                StubInferenceTransport::success(),
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
        let id = ModelRequestId::new("req_xai_adapter_filter").unwrap();
        let key = ClientApiKeyId::new("key_xai_contract").unwrap();
        let policy = RequestPolicyContext::new(
            Arc::new(PreferAdapterAccount(account_id(
                if matches!(state, AdapterState::Available) {
                    "adapter-filter-b"
                } else {
                    "adapter-filter-a"
                },
            ))),
            ExtensionSetReference::new(
                ExtensionSetId::new("adapter-filter-policy".into()).unwrap(),
                Arc::new(TestExtensionLease),
            ),
            id.clone(),
            key.clone(),
            vec![],
        );
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let adapter_plan = gateway_core::engine::upstream_adapter::FrozenUpstreamAdapterPlan::new(
            Arc::new(AccountEligibilityPlan {
                state,
                polls: Arc::clone(&polls),
            }),
            ExtensionSetReference::new(
                ExtensionSetId::new("adapter-filter-plan".into()).unwrap(),
                Arc::new(TestExtensionLease),
            ),
        );
        let scope = Arc::new(FrozenAccountScope::new(
            Arc::new(RuntimeAccountDirectory::new(accounts)),
            ClientRoutingScope::all_accounts(),
        ));
        let operation = if prewarm {
            Operation::Generate(GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object(
                    "openai",
                    object(json!({"model": MODEL, "input": "hello", "generate": false})),
                )
                .unwrap(),
            ))
        } else {
            operation()
        };
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
                .with_request_policy(Some(policy))
                .with_upstream_adapters(Some(adapter_plan)),
            NonZeroU32::MIN,
            SystemTime::now() + Duration::from_secs(5),
            selection_policy(),
            AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
            None,
            CancellationToken::new(),
        );

        let result = provider.execute(request, attempt).await;
        if prewarm && !matches!(state, AdapterState::Available) {
            assert_eq!(
                result
                    .err()
                    .expect("no adapter can accept the prewarm request")
                    .kind(),
                ProviderErrorKind::Unsupported
            );
            assert!(leases.acquired.lock().unwrap().is_empty());
            assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 0);
            continue;
        }
        let mut stream = result.expect("eligible account remains available");
        let selected_account = account_id(if prewarm {
            "adapter-filter-a"
        } else {
            "adapter-filter-b"
        });
        assert_eq!(
            stream.metadata().provider_account_id(),
            &selected_account,
            "{state:?}"
        );
        assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(
            leases
                .acquired
                .lock()
                .unwrap()
                .iter()
                .all(|account| account == &selected_account)
        );
        if prewarm {
            assert_eq!(
                stream.next().await.unwrap().unwrap_err().kind(),
                ProviderErrorKind::Cancelled
            );
            assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
        drop(stream);
    }
}
