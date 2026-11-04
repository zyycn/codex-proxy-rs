//! 互联发现结果的持久恢复、连接失效、手动重查与临时失败退避

use super::*;
use gateway_admin::PeerUpstreamBundle;
use gateway_core::task::WorkerRegistration;

#[tokio::test(start_paused = true)]
async fn unsupported_detection_survives_restart_until_manual_rediscovery() {
    let (mut bundle, accounts, transport, id) = fixture(UpstreamInfoFailure::Unsupported);
    // 入库后的首查与手动刷新使用同一入口，不支持协议也是可保存的正常发现结果
    let initial = bundle.service().refresh_quota(&id).await.unwrap();
    assert!(!initial.upstream.unwrap().discovered);
    let first_worker = worker(&mut bundle);
    tokio::time::advance(Duration::from_secs(86400)).await;
    cycle(&first_worker).await;
    assert_eq!(transport.requests.load(Ordering::SeqCst), 1);
    drop(first_worker);
    drop(bundle);

    let mut restarted = initialize(accounts, transport.clone());
    let info = restarted
        .service()
        .read_quota(&id)
        .await
        .unwrap()
        .unwrap()
        .upstream
        .unwrap();
    assert!(!info.discovered);
    assert_eq!(info.last_error.as_deref(), Some("unsupported"));
    let resumed_worker = worker(&mut restarted);
    cycle(&resumed_worker).await;
    tokio::time::advance(Duration::from_secs(86400)).await;
    cycle(&resumed_worker).await;
    assert_eq!(transport.requests.load(Ordering::SeqCst), 1);
    let summary = restarted
        .service()
        .read_summary(&id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        summary.upstream.unwrap().last_error.as_deref(),
        Some("unsupported")
    );

    *transport.failure.lock().unwrap() = None;
    let discovered = restarted.service().refresh_quota(&id).await.unwrap();
    assert!(discovered.upstream.unwrap().discovered);
    assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_secs(360)).await;
    cycle(&resumed_worker).await;
    assert_eq!(transport.requests.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn unsupported_detection_keeps_manual_refresh_throttling() {
    for retry_after in [None, Some(Duration::from_secs(90))] {
        let (bundle, _, transport, id) = fixture(UpstreamInfoFailure::Unsupported);
        *transport.failure.lock().unwrap() = Some(PeerFetchFailure {
            kind: UpstreamInfoFailure::Unsupported,
            retry_after,
        });
        bundle.service().refresh_quota(&id).await.unwrap();
        let delay = retry_after.unwrap_or(Duration::from_secs(15));
        tokio::time::advance(delay - Duration::from_secs(1)).await;
        bundle.service().refresh_quota(&id).await.unwrap();
        assert_eq!(transport.requests.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        *transport.failure.lock().unwrap() = None;
        assert!(
            bundle
                .service()
                .refresh_quota(&id)
                .await
                .unwrap()
                .upstream
                .unwrap()
                .discovered
        );
        assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test(start_paused = true)]
async fn ordinary_edits_keep_detection_but_proxy_and_credential_changes_reprobe() {
    let (mut bundle, accounts, transport, id) = fixture(UpstreamInfoFailure::Unsupported);
    let worker = worker(&mut bundle);
    // 无识别记录的存量账号由周期任务补查
    cycle(&worker).await;
    let initial = accounts.accounts.lock().unwrap()[0].clone();
    let edited = initial.clone().with_notes(Some("修改备注".into()));
    *accounts.accounts.lock().unwrap() = vec![edited.clone()];
    bundle.service().account_changed(&id).await.unwrap();
    tokio::time::advance(Duration::from_secs(86400)).await;
    cycle(&worker).await;
    assert_eq!(transport.requests.load(Ordering::SeqCst), 1);

    let reconnected = edited.with_outbound_proxy(Some(
        OutboundProxy::parse("http://proxy.invalid:8080").unwrap(),
    ));
    *accounts.accounts.lock().unwrap() = vec![reconnected.clone()];
    bundle.service().account_changed(&id).await.unwrap();
    cycle(&worker).await;
    assert_eq!(transport.requests.load(Ordering::SeqCst), 2);

    let replaced = ProviderAccount::new(
        id.clone(),
        initial.provider().clone(),
        "new credential".into(),
        None,
        "peer_test".into(),
        CredentialRevision::new(2).unwrap(),
        None,
    )
    .with_outbound_proxy(reconnected.outbound_proxy().cloned())
    .with_account_facts(
        true,
        CredentialState::Ready,
        QuotaState::unknown(),
        None,
        None,
    );
    *accounts.accounts.lock().unwrap() = vec![replaced];
    bundle.service().account_changed(&id).await.unwrap();
    cycle(&worker).await;
    assert_eq!(transport.requests.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn uncertain_discovery_failures_retry_without_becoming_unsupported() {
    for failure in [
        UpstreamInfoFailure::Unauthorized,
        UpstreamInfoFailure::Unavailable,
        UpstreamInfoFailure::RateLimited,
        UpstreamInfoFailure::InvalidResponse,
    ] {
        let (mut bundle, _, transport, id) = fixture(failure);
        let worker = worker(&mut bundle);
        cycle(&worker).await;
        let info = bundle
            .service()
            .read_quota(&id)
            .await
            .unwrap()
            .unwrap()
            .upstream
            .unwrap();
        assert_eq!(info.last_error.as_deref(), Some(failure.as_str()));
        assert!(!info.discovered);
        cycle(&worker).await;
        assert_eq!(transport.requests.load(Ordering::SeqCst), 1);
        *transport.failure.lock().unwrap() = None;
        tokio::time::advance(Duration::from_secs(
            if failure == UpstreamInfoFailure::Unauthorized {
                1800
            } else {
                30
            },
        ))
        .await;
        cycle(&worker).await;
        assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
        assert!(
            bundle
                .service()
                .read_quota(&id)
                .await
                .unwrap()
                .unwrap()
                .upstream
                .unwrap()
                .discovered
        );
    }
}

#[tokio::test(start_paused = true)]
async fn known_peer_failures_keep_identity_and_only_revocation_discards_shared_data() {
    for failure in [
        UpstreamInfoFailure::Unauthorized,
        UpstreamInfoFailure::Unsupported,
        UpstreamInfoFailure::Unavailable,
        UpstreamInfoFailure::RateLimited,
        UpstreamInfoFailure::InvalidResponse,
    ] {
        let (bundle, accounts, transport, id) = fixture(failure);
        *transport.failure.lock().unwrap() = None;
        let before = bundle.service().refresh_quota(&id).await.unwrap();
        assert!(!before.windows.is_empty());
        tokio::time::advance(Duration::from_secs(15)).await;
        *transport.failure.lock().unwrap() = Some(failure.into());
        let result = bundle.service().refresh_quota(&id).await;
        assert_eq!(result.is_ok(), failure == UpstreamInfoFailure::Unsupported);
        let clears_data = matches!(
            failure,
            UpstreamInfoFailure::Unauthorized | UpstreamInfoFailure::Unsupported
        );
        let quota = bundle.service().read_quota(&id).await.unwrap().unwrap();
        let info = quota.upstream.as_ref().unwrap();
        assert!(info.discovered);
        assert_eq!(info.last_error.as_deref(), Some(failure.as_str()));
        assert_eq!(quota.windows.is_empty(), clears_data);
        assert_eq!(info.observed_at.is_none(), clears_data);
        assert_eq!(
            bundle
                .service()
                .read_summary(&id)
                .await
                .unwrap()
                .unwrap()
                .upstream,
            quota.upstream
        );
        let account = accounts.accounts.lock().unwrap()[0].clone();
        assert_eq!(
            bundle
                .scheduling_source()
                .signal(&account, "model", SystemTime::now())
                .is_none(),
            clears_data
        );

        drop(bundle);
        let restarted = initialize(accounts, transport);
        let restored = restarted.service().read_quota(&id).await.unwrap().unwrap();
        assert!(restored.upstream.as_ref().unwrap().discovered);
        assert_eq!(restored.windows.is_empty(), clears_data);
        if clears_data {
            assert_eq!(
                restored.upstream.as_ref().unwrap().last_error.as_deref(),
                Some(failure.as_str())
            );
        }
        assert_eq!(
            restarted
                .service()
                .read_summary(&id)
                .await
                .unwrap()
                .unwrap()
                .upstream,
            restored.upstream
        );
    }
}

#[tokio::test(start_paused = true)]
async fn known_peer_identity_is_scoped_to_the_connection_after_revocation() {
    for failure in [
        UpstreamInfoFailure::Unauthorized,
        UpstreamInfoFailure::Unsupported,
    ] {
        let (bundle, accounts, transport, id) = fixture(failure);
        *transport.failure.lock().unwrap() = None;
        bundle.service().refresh_quota(&id).await.unwrap();
        tokio::time::advance(Duration::from_secs(15)).await;
        *transport.failure.lock().unwrap() = Some(failure.into());
        let _result = bundle.service().refresh_quota(&id).await;
        let initial = accounts.accounts.lock().unwrap()[0].clone();
        let edited = initial.clone().with_notes(Some("保留连接身份".into()));
        *accounts.accounts.lock().unwrap() = vec![edited.clone()];
        bundle.service().account_changed(&id).await.unwrap();
        assert!(
            bundle
                .service()
                .read_summary(&id)
                .await
                .unwrap()
                .unwrap()
                .upstream
                .unwrap()
                .discovered
        );

        // 地址和密钥修改推进凭据版本，出口变化则改变同一版本的连接身份
        for replacement in [
            edited.with_outbound_proxy(Some(
                OutboundProxy::parse("http://other.invalid:8080").unwrap(),
            )),
            ProviderAccount::new(
                id.clone(),
                initial.provider().clone(),
                "替换连接".into(),
                None,
                "peer_test".into(),
                CredentialRevision::new(2).unwrap(),
                None,
            )
            .with_account_facts(
                true,
                CredentialState::Ready,
                QuotaState::unknown(),
                None,
                None,
            ),
        ] {
            *accounts.accounts.lock().unwrap() = vec![replacement];
            bundle.service().account_changed(&id).await.unwrap();
            let detail = bundle.service().read_quota(&id).await.unwrap().unwrap();
            assert!(!detail.upstream.as_ref().unwrap().discovered);
            assert!(detail.upstream.as_ref().unwrap().last_error.is_none());
            assert_eq!(
                bundle
                    .service()
                    .read_summary(&id)
                    .await
                    .unwrap()
                    .unwrap()
                    .upstream,
                detail.upstream
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn manual_rediscovery_does_not_wait_for_authorization_backoff_but_respects_retry_after() {
    for retry_after in [None, Some(Duration::from_secs(90))] {
        let (mut bundle, _, transport, id) = fixture(UpstreamInfoFailure::Unauthorized);
        *transport.failure.lock().unwrap() = None;
        bundle.service().refresh_quota(&id).await.unwrap();
        tokio::time::advance(Duration::from_secs(15)).await;
        *transport.failure.lock().unwrap() = Some(PeerFetchFailure {
            kind: UpstreamInfoFailure::Unauthorized,
            retry_after,
        });
        assert!(bundle.service().refresh_quota(&id).await.is_err());
        let worker = worker(&mut bundle);
        *transport.failure.lock().unwrap() = None;
        let delay = retry_after.unwrap_or(Duration::from_secs(15));
        tokio::time::advance(delay - Duration::from_secs(1)).await;
        assert!(bundle.service().refresh_quota(&id).await.is_err());
        assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
        tokio::time::advance(Duration::from_secs(1)).await;
        cycle(&worker).await;
        assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
        let recovered = bundle.service().refresh_quota(&id).await.unwrap();
        assert_eq!(transport.requests.load(Ordering::SeqCst), 3);
        assert!(recovered.upstream.as_ref().unwrap().discovered);
        assert!(recovered.upstream.as_ref().unwrap().last_error.is_none());
        assert!(!recovered.windows.is_empty());
    }
}

fn fixture(
    failure: UpstreamInfoFailure,
) -> (
    PeerUpstreamBundle,
    Arc<TestAccounts>,
    Arc<DetectionTransport>,
    ProviderAccountId,
) {
    let record = account("acct_peer", "openai", "peer_test");
    let id = record.id().clone();
    let accounts = Arc::new(TestAccounts {
        accounts: Mutex::new(vec![record]),
        records: Mutex::default(),
        failed_provider: None,
    });
    let transport = Arc::new(DetectionTransport {
        requests: AtomicUsize::new(0),
        failure: Mutex::new(Some(failure.into())),
        success: TestTransport(Mutex::default()),
    });
    (
        initialize(accounts.clone(), transport.clone()),
        accounts,
        transport,
        id,
    )
}

fn initialize(
    accounts: Arc<TestAccounts>,
    transport: Arc<DetectionTransport>,
) -> PeerUpstreamBundle {
    let mut provider = UnavailableProvider::new("openai");
    provider.peer_enabled = true;
    let registry =
        ProviderAdminRegistry::new([Arc::new(provider) as Arc<dyn ProviderAdmin>]).unwrap();
    initialize_peer_upstream(accounts, &registry, transport).unwrap()
}

fn worker(bundle: &mut PeerUpstreamBundle) -> WorkerRegistration {
    let WorkerContribution::Registration(worker) = bundle.take_worker_contributions().remove(0)
    else {
        panic!("scheduled worker")
    };
    worker
}

async fn cycle(worker: &WorkerRegistration) {
    let WorkerRunnable::Scheduled { task, .. } = &worker.runnable else {
        panic!("scheduled worker")
    };
    task.run_cycle(WorkerCycleContext::new(
        worker.id.clone(),
        None,
        CancellationToken::new(),
    ))
    .await
    .unwrap();
}

struct DetectionTransport {
    requests: AtomicUsize,
    failure: Mutex<Option<PeerFetchFailure>>,
    success: TestTransport,
}

#[async_trait]
impl PeerTransport for DetectionTransport {
    async fn fetch(
        &self,
        account: &ProviderAccount,
        connection: &PeerConnection,
    ) -> Result<Vec<u8>, PeerFetchFailure> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        if let Some(failure) = self.failure.lock().unwrap().as_ref() {
            return Err(PeerFetchFailure {
                kind: failure.kind,
                retry_after: failure.retry_after,
            });
        }
        self.success.fetch(account, connection).await
    }
}
