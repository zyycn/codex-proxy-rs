//! OpenAI 凭据适配、公共互联服务、Host 传输与调度只读端口的集成验证

use super::*;
use gateway_core::{
    account::{
        LoadedCredential, UpstreamConnection, UpstreamInfoClaim, UpstreamInfoCompletion,
        UpstreamInfoFailure, UpstreamInfoPlan, UpstreamInfoRecord, UpstreamInfoStore,
    },
    error::{StoreError, StoreErrorKind},
    task::WorkerCycleContext,
};
use provider_openai::credential::ResponsesTransport;

pub(super) struct MemoryUpstreamInfoStore {
    accounts: Arc<MemoryAccountStore>,
    records: Mutex<BTreeMap<ProviderAccountId, UpstreamInfoRecord>>,
    claims: Mutex<BTreeMap<ProviderAccountId, uuid::Uuid>>,
    snapshot_reads: std::sync::atomic::AtomicUsize,
    largest_snapshot_read: std::sync::atomic::AtomicUsize,
}

impl MemoryUpstreamInfoStore {
    pub(super) fn new(accounts: Arc<MemoryAccountStore>) -> Self {
        Self {
            accounts,
            records: Mutex::new(BTreeMap::new()),
            claims: Mutex::new(BTreeMap::new()),
            snapshot_reads: std::sync::atomic::AtomicUsize::new(0),
            largest_snapshot_read: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    async fn current(&self, connection: &UpstreamConnection) -> Result<(), StoreError> {
        let account = self
            .accounts
            .get_account(&connection.account_id)
            .await?
            .ok_or_else(|| StoreError::new(StoreErrorKind::InvalidData))?;
        if UpstreamConnection::for_account(&account) != *connection {
            return Err(StoreError::new(StoreErrorKind::Conflict));
        }
        Ok(())
    }

    fn due_now(&self, id: &ProviderAccountId) {
        self.records
            .lock()
            .unwrap()
            .get_mut(id)
            .unwrap()
            .plan
            .next_attempt_at = SystemTime::now() - Duration::from_secs(1);
    }
}

#[async_trait::async_trait]
impl UpstreamInfoStore for MemoryUpstreamInfoStore {
    async fn list_candidates(
        &self,
        provider: &ProviderKind,
        authentication_kinds: &[&str],
        after: Option<&ProviderAccountId>,
        limit: u32,
    ) -> Result<Vec<LoadedCredential>, StoreError> {
        let accounts = self.accounts.list_for_provider(provider).await?;
        let mut result = Vec::new();
        for account in accounts
            .into_iter()
            .filter(|account| {
                authentication_kinds.contains(&account.authentication_kind())
                    && after.is_none_or(|after| account.id() > after)
            })
            .take(limit as usize)
        {
            result.push(self.accounts.load_current_credential(account.id()).await?);
        }
        Ok(result)
    }

    async fn reconcile(
        &self,
        connection: &UpstreamConnection,
        enabled: bool,
        now: SystemTime,
    ) -> Result<(), StoreError> {
        self.current(connection).await?;
        let mut claims = self.claims.lock().unwrap();
        let mut records = self.records.lock().unwrap();
        if !enabled {
            claims.remove(&connection.account_id);
            records.remove(&connection.account_id);
            return Ok(());
        }
        if records
            .get(&connection.account_id)
            .is_none_or(|record| record.plan.connection != *connection)
        {
            records.insert(
                connection.account_id.clone(),
                UpstreamInfoRecord {
                    plan: UpstreamInfoPlan {
                        connection: connection.clone(),
                        request_generation: 1,
                        completed_generation: 0,
                        next_attempt_at: now,
                        claimed_until: None,
                        last_attempt_at: None,
                        last_success_at: None,
                        last_error: None,
                        consecutive_failures: 0,
                    },
                    snapshot: None,
                },
            );
        }
        Ok(())
    }

    async fn request_refresh(
        &self,
        connection: &UpstreamConnection,
        now: SystemTime,
        minimum_interval: Duration,
    ) -> Result<UpstreamInfoPlan, StoreError> {
        self.current(connection).await?;
        let mut records = self.records.lock().unwrap();
        let record = records
            .get_mut(&connection.account_id)
            .ok_or_else(|| StoreError::new(StoreErrorKind::InvalidData))?;
        if record.plan.request_generation == record.plan.completed_generation {
            record.plan.request_generation += 1;
            let earliest = record
                .plan
                .last_attempt_at
                .map_or(now, |time| time + minimum_interval)
                .max(now);
            record.plan.next_attempt_at = if record.plan.last_error.is_some() {
                record.plan.next_attempt_at.max(earliest)
            } else {
                earliest
            };
        }
        Ok(record.plan.clone())
    }

    async fn read_plans(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<UpstreamInfoPlan>, StoreError> {
        let records = self.records.lock().unwrap();
        Ok(accounts
            .iter()
            .filter_map(|id| records.get(id).map(|record| record.plan.clone()))
            .collect())
    }

    async fn read(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<UpstreamInfoRecord>, StoreError> {
        self.snapshot_reads
            .fetch_add(accounts.len(), std::sync::atomic::Ordering::Relaxed);
        self.largest_snapshot_read
            .fetch_max(accounts.len(), std::sync::atomic::Ordering::Relaxed);
        let records = self.records.lock().unwrap();
        Ok(accounts
            .iter()
            .filter_map(|id| records.get(id).cloned())
            .collect())
    }

    async fn claim_due(
        &self,
        _: &ProviderKind,
        now: SystemTime,
        limit: u32,
        lease_duration: Duration,
    ) -> Result<Vec<UpstreamInfoClaim>, StoreError> {
        let mut claims = self.claims.lock().unwrap();
        let mut records = self.records.lock().unwrap();
        Ok(records
            .values_mut()
            .filter(|record| {
                record.plan.next_attempt_at <= now
                    && record.plan.claimed_until.is_none_or(|expiry| expiry <= now)
            })
            .take(limit as usize)
            .map(|record| {
                if record.plan.request_generation == record.plan.completed_generation
                    || record.plan.claimed_until.is_some()
                {
                    record.plan.request_generation += 1;
                }
                record.plan.claimed_until = Some(now + lease_duration);
                record.plan.last_attempt_at = Some(now);
                let claim_token = uuid::Uuid::new_v4();
                claims.insert(record.plan.connection.account_id.clone(), claim_token);
                UpstreamInfoClaim {
                    plan: record.plan.clone(),
                    claim_token,
                }
            })
            .collect())
    }

    async fn complete(&self, completion: UpstreamInfoCompletion) -> Result<bool, StoreError> {
        if self.current(&completion.connection).await.is_err() {
            return Ok(false);
        }
        let mut claims = self.claims.lock().unwrap();
        let mut records = self.records.lock().unwrap();
        let Some(record) = records.get_mut(&completion.connection.account_id) else {
            return Ok(false);
        };
        if claims.get(&completion.connection.account_id) != Some(&completion.claim_token)
            || record
                .plan
                .claimed_until
                .is_none_or(|expiry| expiry <= completion.completed_at)
            || record.plan.connection != completion.connection
            || record.plan.request_generation != completion.request_generation
        {
            return Ok(false);
        }
        claims.remove(&completion.connection.account_id);
        record.plan.completed_generation = completion.request_generation;
        record.plan.claimed_until = None;
        record.plan.next_attempt_at = completion.next_attempt_at;
        match completion.result {
            Ok(snapshot) => {
                record.snapshot = Some(snapshot);
                record.plan.last_success_at = Some(completion.completed_at);
                record.plan.last_error = None;
                record.plan.consecutive_failures = 0;
            }
            Err(error) => {
                record.plan.last_error = Some(error);
                record.plan.consecutive_failures += 1;
                if error == UpstreamInfoFailure::Unauthorized {
                    record.snapshot = None;
                }
            }
        }
        Ok(true)
    }
}

fn peer_document() -> Value {
    let now = Utc::now();
    json!({
        "object":"cpr.peer", "schema_version":1,
        "node":{"id":"synthetic-peer","name":"测试上游","software":"cpr"},
        "observed_at":now.to_rfc3339(), "expires_at":(now + chrono::Duration::minutes(5)).to_rfc3339(),
        "capabilities":["key_usage", "pool_summary"],
        "usage":{"status":"available","observed_at":now.to_rfc3339(),"data":{"unit":"USD","daily":{"total":"10","used":"2","remaining":"8","resetsAt":(now + chrono::Duration::days(1)).to_rfc3339()},"weekly":{"total":"50","used":"5","remaining":"45","resetsAt":(now + chrono::Duration::days(7)).to_rfc3339()}}},
        "pool":{"disclosure":"summary","account_count":2,"states":{"ready":2,"cooldown":0,"exhausted":0,"disabled":0,"error":0,"unknown":0},"plans":[{"name":"plus","count":2}]},
        "quota":{"disclosure":"none"},"models":[]
    })
}

async fn fixture(
    server: &MockServer,
) -> (
    PeerFixture,
    Arc<MemoryAccountStore>,
    Arc<MemoryUpstreamInfoStore>,
    ProviderAccountId,
    TestOpenAiConfig,
) {
    let accounts = Arc::new(MemoryAccountStore::default());
    accounts
        .seed_api_key(
            "acct_peer",
            format!("{}/custom/v1", server.uri()),
            ResponsesTransport::Http,
        )
        .await;
    let id = ProviderAccountId::new("acct_peer").unwrap();
    let upstream = Arc::new(MemoryUpstreamInfoStore::new(accounts.clone()));
    let config = valid_config();
    let bundle = peer_fixture(config.config.clone(), accounts.clone(), upstream.clone()).await;
    (bundle, accounts, upstream, id, config)
}

fn worker(bundle: &mut PeerFixture) -> gateway_core::task::WorkerRegistration {
    bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|contribution| match contribution {
            WorkerContribution::Registration(registration)
                if registration.id.owner() == "peer-info" =>
            {
                Some(registration)
            }
            _ => None,
        })
        .unwrap()
}

async fn cycle(worker: &gateway_core::task::WorkerRegistration) {
    let WorkerRunnable::Scheduled { task, .. } = &worker.runnable else {
        panic!("scheduled")
    };
    task.run_cycle(WorkerCycleContext::new(
        worker.id.clone(),
        None,
        CancellationToken::new(),
    ))
    .await
    .unwrap();
}

async fn quota(
    bundle: &PeerFixture,
    id: &ProviderAccountId,
) -> gateway_admin::model::provider_credentials::ProviderQuota {
    match bundle.peer.service().read_quota(id).await.unwrap() {
        Some(quota) => quota,
        None => bundle
            .provider
            .admin_provider()
            .quota(ProviderQuotaRequest {
                account_id: id.clone(),
                refresh: false,
                rolling_usage: None,
            })
            .await
            .unwrap(),
    }
}

#[tokio::test]
async fn refresh_and_reads_are_local_and_worker_owns_the_upstream_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom/v1/cpr/link"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer sk-api-test-only",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer_document()))
        .expect(1)
        .mount(&server)
        .await;
    let (mut bundle, _, _, id, _config) = fixture(&server).await;
    let admin = bundle.admin_provider();
    admin.account_changed(&id).await.unwrap();
    let first = admin.request_refresh(&id).await.unwrap();
    let second = admin.request_refresh(&id).await.unwrap();
    assert_eq!(first.request_generation, second.request_generation);
    assert!(quota(&bundle, &id).await.upstream.unwrap().node.is_none());
    assert!(server.received_requests().await.unwrap().is_empty());
    cycle(&worker(&mut bundle)).await;
    let observed = quota(&bundle, &id).await;
    assert_eq!(observed.upstream.unwrap().node.unwrap().name, "测试上游");
    assert_eq!(observed.windows[0].used_percent, Some(20.0));
    assert_eq!(
        observed.windows[0].local_usage_attribution,
        QuotaLocalUsageAttribution::AccountWide
    );
    assert!(observed.windows[0].local_usage.is_none());
    assert_eq!(
        observed.windows[0]
            .upstream
            .as_ref()
            .unwrap()
            .amount
            .as_ref()
            .unwrap()
            .remaining
            .as_deref(),
        Some("8")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn native_windows_preserve_account_and_bucket_identity_without_pool_averaging() {
    let server = MockServer::start().await;
    let mut document = peer_document();
    document["capabilities"] = serde_json::json!(["quota_windows"]);
    let observed_at = document["observed_at"].clone();
    let expires_at = document["expires_at"].clone();
    let window = |bucket: &str, percent: f64| {
        serde_json::json!({
            "key": "weekly", "group": "model", "label": "周额度",
            "limit_id": bucket, "limit_name": bucket, "used_percent": percent,
            "window_seconds": 604800, "limit_reached": false
        })
    };
    document["quota"] = serde_json::json!({
        "disclosure": "accounts",
        "accounts": [
            { "id": "first", "models": ["gpt-5.4"], "observed_at": observed_at,
              "expires_at": expires_at, "windows": [window("Codex", 10.0), window("Other", 30.0)] },
            { "id": "second", "models": ["gpt-5.4"], "observed_at": observed_at,
              "expires_at": expires_at, "windows": [window("Codex", 90.0)] }
        ]
    });
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(document))
        .mount(&server)
        .await;
    let (mut bundle, _, _, id, _) = fixture(&server).await;
    cycle(&worker(&mut bundle)).await;
    let observed = quota(&bundle, &id).await;
    assert_eq!(observed.windows.len(), 3);
    assert_eq!(
        observed
            .windows
            .iter()
            .map(|window| window
                .upstream
                .as_ref()
                .and_then(|upstream| upstream.account_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![Some("first"), Some("first"), Some("second")]
    );
    assert_eq!(
        observed
            .windows
            .iter()
            .map(|window| window.used_percent)
            .collect::<Vec<_>>(),
        vec![Some(10.0), Some(30.0), Some(90.0)]
    );
    let scopes: std::collections::BTreeSet<_> = observed
        .windows
        .iter()
        .filter_map(|window| window.limit_id.as_deref())
        .collect();
    assert_eq!(scopes.len(), 3);
    assert!(
        observed
            .windows
            .iter()
            .all(|window| window.local_usage.is_none()
                && window.upstream.as_ref().unwrap().amount.is_none())
    );
}

#[tokio::test]
async fn transient_failure_preserves_observation_and_revocation_erases_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer_document()))
        .mount(&server)
        .await;
    let (mut bundle, _, store, id, _config) = fixture(&server).await;
    let worker = worker(&mut bundle);
    cycle(&worker).await;
    let observed_at = quota(&bundle, &id).await.observed_at;
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    store.due_now(&id);
    cycle(&worker).await;
    let failed = quota(&bundle, &id).await;
    assert_eq!(failed.observed_at, observed_at);
    assert_eq!(
        failed.upstream.unwrap().last_error.as_deref(),
        Some("unavailable")
    );
    assert_eq!(failed.windows.len(), 2);
    let receipt = bundle.admin_provider().request_refresh(&id).await.unwrap();
    assert!(receipt.request_generation > 0);
    assert!(
        store.read(std::slice::from_ref(&id)).await.unwrap()[0]
            .plan
            .next_attempt_at
            > SystemTime::now()
    );
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    store.due_now(&id);
    cycle(&worker).await;
    let revoked = quota(&bundle, &id).await;
    assert!(revoked.windows.is_empty());
    assert!(revoked.upstream.unwrap().node.is_none());
}

async fn wait_for_request(server: &MockServer) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !server.received_requests().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn disabling_during_an_in_flight_request_fences_the_late_observation() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_json(peer_document()),
        )
        .mount(&server)
        .await;
    let (mut bundle, accounts, store, id, _config) = fixture(&server).await;
    let worker = Arc::new(worker(&mut bundle));
    let running = tokio::spawn({
        let worker = worker.clone();
        async move {
            cycle(&worker).await;
        }
    });
    wait_for_request(&server).await;
    accounts.set_enabled(&id, false).await.unwrap();
    bundle.admin_provider().account_changed(&id).await.unwrap();
    running.await.unwrap();
    assert!(
        store
            .read(std::slice::from_ref(&id))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(quota(&bundle, &id).await.upstream.is_none());
}

#[tokio::test]
async fn cancellation_leaves_the_persisted_claim_for_restart_recovery() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(2))
                .set_body_json(peer_document()),
        )
        .mount(&server)
        .await;
    let (mut bundle, _, store, id, _config) = fixture(&server).await;
    let worker = worker(&mut bundle);
    let cancellation = CancellationToken::new();
    let context = WorkerCycleContext::new(worker.id.clone(), None, cancellation.clone());
    let running = tokio::spawn(async move {
        let WorkerRunnable::Scheduled { task, .. } = worker.runnable else {
            panic!("scheduled")
        };
        task.run_cycle(context).await.unwrap();
    });
    wait_for_request(&server).await;
    cancellation.cancel();
    tokio::time::timeout(Duration::from_millis(100), running)
        .await
        .unwrap()
        .unwrap();
    let record = store
        .read(std::slice::from_ref(&id))
        .await
        .unwrap()
        .remove(0);
    assert!(record.plan.claimed_until.is_some());
    assert_eq!(record.plan.completed_generation, 0);
    assert!(record.snapshot.is_none());
}

#[tokio::test]
async fn invalid_or_redirected_responses_never_replace_the_last_valid_snapshot() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer_document()))
        .mount(&server)
        .await;
    let (mut bundle, _, store, id, _config) = fixture(&server).await;
    let worker = worker(&mut bundle);
    cycle(&worker).await;
    let observed_at = quota(&bundle, &id).await.observed_at;
    let mut wrong_schema = peer_document();
    wrong_schema["schema_version"] = json!(2);
    let mut wrong_object = peer_document();
    wrong_object["object"] = json!("other.peer");
    let mut negative = peer_document();
    negative["usage"]["data"]["daily"]["used"] = json!("-20");
    let mut future = peer_document();
    future["observed_at"] = json!((Utc::now() + chrono::Duration::minutes(10)).to_rfc3339());
    let mut huge_pool = peer_document();
    huge_pool["pool"]["account_count"] = json!(100_000_000);
    huge_pool["pool"]["states"]["ready"] = json!(100_000_000);
    let mut conflicting_states = peer_document();
    conflicting_states["pool"]["states"]["ready"] = json!(3);
    let mut conflicting_plans = peer_document();
    conflicting_plans["pool"]["plans"] =
        json!([{"name":"Pro","count":2},{"name":"Plus","count":2}]);
    let responses = [
        ResponseTemplate::new(200).set_body_json(huge_pool),
        ResponseTemplate::new(200).set_body_json(conflicting_states),
        ResponseTemplate::new(200).set_body_json(conflicting_plans),
        ResponseTemplate::new(200).set_body_json(wrong_schema),
        ResponseTemplate::new(200).set_body_json(wrong_object),
        ResponseTemplate::new(200).set_body_json(negative),
        ResponseTemplate::new(200).set_body_json(future),
        ResponseTemplate::new(200).set_body_raw(
            "x".repeat(gateway_protocol::peer::MAX_RESPONSE_BYTES + 1),
            "application/json",
        ),
        ResponseTemplate::new(302)
            .insert_header("location", format!("{}/never-follow", server.uri())),
    ];
    for response in responses {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/custom/v1/cpr/link"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/never-follow"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        store.due_now(&id);
        cycle(&worker).await;
        let quota = quota(&bundle, &id).await;
        assert_eq!(quota.observed_at, observed_at);
        assert_eq!(quota.windows.len(), 2);
        assert!(quota.upstream.unwrap().last_error.is_some());
        server.verify().await;
    }
}

#[tokio::test]
async fn smart_prefers_another_account_when_key_budget_is_exhausted_without_pool_disclosure() {
    let server = MockServer::start().await;
    let mut peer = peer_document();
    peer["capabilities"] = json!(["key_usage"]);
    peer["pool"] = json!({"disclosure":"none"});
    peer["usage"]["data"]["daily"]["used"] = json!("10.0000");
    peer["usage"]["data"]["daily"]["remaining"] = json!("0.0000");
    Mock::given(method("GET"))
        .and(path("/custom/v1/cpr/link"))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/available/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_SESSION_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/custom/v1/responses"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let (mut bundle, accounts, _, _, _config) = fixture(&server).await;
    accounts
        .seed_api_key(
            "acct_available",
            format!("{}/available/v1", server.uri()),
            ResponsesTransport::Http,
        )
        .await;
    cycle(&worker(&mut bundle)).await;
    let provider = ProviderKind::new("openai").unwrap();
    let model = UpstreamModelId::new("gpt-5.4").unwrap();
    let scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([
            (
                ProviderAccountId::new("acct_peer").unwrap(),
                RuntimeAccount::new(provider.clone(), BTreeSet::new()),
            ),
            (
                ProviderAccountId::new("acct_available").unwrap(),
                RuntimeAccount::new(provider.clone(), BTreeSet::new()),
            ),
        ]))),
        ClientRoutingScope::all_accounts(),
    ));
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model":"gpt-5.4", "input":"budget routing"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap(),
    ));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(2, "smart", Default::default(), None, None),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            model,
            ModelCapabilities::new(BTreeSet::from([operation.kind()]), Some(32_000))
                .with_upstream_feature_validation(),
        )],
        Vec::new(),
    )
    .unwrap();
    let plan = snapshot
        .plan(
            &PublicModelId::new("gpt-5.4").unwrap(),
            &operation,
            scope.clone(),
            &RoutingContext::default(),
        )
        .unwrap();
    let context = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_peer_smart").unwrap(),
            ClientApiKeyId::new("key_peer_smart").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(5),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
        None,
        CancellationToken::new(),
    );
    let mut response = bundle
        .core_provider()
        .execute(
            ProviderRequest::new(operation, plan.candidates()[0].clone()),
            context,
        )
        .await
        .unwrap();
    assert_eq!(
        response.metadata().provider_account_id().as_str(),
        "acct_available"
    );
    while let Some(event) = response.next().await {
        event.unwrap();
    }
    server.verify().await;
}

#[tokio::test]
async fn lists_reuse_versioned_summaries_and_details_keep_node_windows() {
    let server = MockServer::start().await;
    let mut document = peer_document();
    document["capabilities"] = json!(["key_usage", "pool_summary", "quota_windows"]);
    document["quota"] = json!({"disclosure":"accounts","accounts":[{
        "id":"node-one", "models":["gpt-5.4"],
        "observed_at":document["observed_at"], "expires_at":document["expires_at"],
        "windows":[{"key":"weekly","group":"general","label":"周额度","used_percent":40,"limit_reached":false}]
    }]});
    Mock::given(method("GET"))
        .and(path("/custom/v1/cpr/link"))
        .respond_with(ResponseTemplate::new(200).set_body_json(document))
        .expect(1)
        .mount(&server)
        .await;
    let (mut bundle, accounts, store, id, config) = fixture(&server).await;
    let worker = worker(&mut bundle);
    cycle(&worker).await;
    let account = account_record(&accounts.account(id.as_str()).unwrap());
    let requests = [ProviderQuotaReadRequest {
        account: &account,
        rolling_usage: None,
    }];
    let read_count = store
        .snapshot_reads
        .load(std::sync::atomic::Ordering::Relaxed);
    for _ in 0..3 {
        let summary = bundle
            .admin_provider()
            .read_quotas(&requests)
            .await
            .pop()
            .unwrap()
            .unwrap();
        assert_eq!(summary.windows.len(), 2);
        assert!(
            summary.windows.iter().all(|window| window
                .upstream
                .as_ref()
                .unwrap()
                .account_id
                .is_none())
        );
        assert!(summary.upstream.as_ref().unwrap().discovered);
        assert_eq!(summary.upstream.unwrap().pool.unwrap().account_count, 2);
        cycle(&worker).await;
    }
    assert_eq!(
        store
            .snapshot_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        read_count
    );
    let detail = quota(&bundle, &id).await;
    assert_eq!(detail.windows.len(), 3);
    assert_eq!(
        detail.windows[2]
            .upstream
            .as_ref()
            .unwrap()
            .account_id
            .as_deref(),
        Some("node-one")
    );

    let restored = peer_fixture(config.config.clone(), accounts, store.clone()).await;
    let summary = restored
        .admin_provider()
        .read_quotas(&requests)
        .await
        .pop()
        .unwrap()
        .unwrap();
    assert_eq!(summary.windows.len(), 2);
    let read_count = store
        .snapshot_reads
        .load(std::sync::atomic::Ordering::Relaxed);
    {
        let mut records = store.records.lock().unwrap();
        let record = records.get_mut(&id).unwrap();
        record.plan.completed_generation += 1;
        let mut snapshot = record
            .snapshot
            .as_ref()
            .unwrap()
            .expose_to_provider()
            .clone();
        snapshot.get_mut("node").unwrap()["name"] = json!("更新后的节点");
        record.snapshot = Some(OpaqueProviderData::new(snapshot));
    }
    let summary = restored
        .admin_provider()
        .read_quotas(&requests)
        .await
        .pop()
        .unwrap()
        .unwrap();
    assert_eq!(summary.upstream.unwrap().node.unwrap().name, "更新后的节点");
    assert_eq!(
        store
            .snapshot_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        read_count + 1
    );
    {
        let mut records = store.records.lock().unwrap();
        let record = records.get_mut(&id).unwrap();
        record.plan.last_error = Some(UpstreamInfoFailure::Unauthorized);
        record.snapshot = None;
    }
    let summary = restored
        .admin_provider()
        .read_quotas(&requests)
        .await
        .pop()
        .unwrap()
        .unwrap();
    assert!(summary.windows.is_empty());
    let info = summary.upstream.unwrap();
    assert!(info.discovered);
    assert!(info.node.is_none());
    assert_eq!(
        store
            .snapshot_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        read_count + 1
    );
}

#[tokio::test]
async fn ordinary_api_upstreams_remain_undiscovered_and_keep_the_detection_backoff() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom/v1/cpr/link"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let (mut bundle, _, store, id, _config) = fixture(&server).await;
    let worker = worker(&mut bundle);
    cycle(&worker).await;
    cycle(&worker).await;
    let info = quota(&bundle, &id).await.upstream.unwrap();
    assert!(!info.discovered);
    assert!(info.node.is_none());
    assert_eq!(info.last_error.as_deref(), Some("unsupported"));
    let plan = store
        .read_plans(std::slice::from_ref(&id))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(plan.next_attempt_at > SystemTime::now() + Duration::from_secs(29 * 60));
}

#[tokio::test]
async fn cold_lists_bound_full_snapshot_batches_and_reuse_every_hydrated_summary() {
    let server = MockServer::start().await;
    let (bundle, accounts, store, first_id, _config) = fixture(&server).await;
    let mut records = Vec::new();
    for index in 0..20 {
        let id = if index == 0 {
            first_id.clone()
        } else {
            let id = format!("acct_peer_{index}");
            accounts
                .seed_api_key(
                    &id,
                    format!("{}/custom/v1", server.uri()),
                    ResponsesTransport::Http,
                )
                .await;
            ProviderAccountId::new(id).unwrap()
        };
        let account = accounts.account(id.as_str()).unwrap();
        let connection = UpstreamConnection::for_account(&account);
        let now = SystemTime::now();
        store.reconcile(&connection, true, now).await.unwrap();
        let mut document = peer_document();
        document["node"]["name"] = json!(format!("peer-{index}"));
        {
            let mut persisted = store.records.lock().unwrap();
            let record = persisted.get_mut(&id).unwrap();
            record.plan.completed_generation = record.plan.request_generation;
            record.plan.last_success_at = Some(now);
            record.snapshot = Some(OpaqueProviderData::new(
                document.as_object().unwrap().clone(),
            ));
        }
        records.push(account_record(&account));
    }
    let requests = records
        .iter()
        .map(|account| ProviderQuotaReadRequest {
            account,
            rolling_usage: None,
        })
        .collect::<Vec<_>>();
    for _ in 0..3 {
        let quotas = bundle.admin_provider().read_quotas(&requests).await;
        assert_eq!(quotas.len(), records.len());
        for (index, quota) in quotas.into_iter().enumerate() {
            let quota = quota.unwrap();
            assert_eq!(quota.windows.len(), 2);
            assert_eq!(
                quota.upstream.unwrap().node.unwrap().name,
                format!("peer-{index}")
            );
        }
        assert_eq!(
            store
                .snapshot_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            records.len()
        );
    }
    assert!(
        store
            .largest_snapshot_read
            .load(std::sync::atomic::Ordering::Relaxed)
            <= 2
    );

    {
        let mut persisted = store.records.lock().unwrap();
        for account in [&records[0], records.last().unwrap()] {
            let record = persisted
                .get_mut(&ProviderAccountId::new(account.id.clone()).unwrap())
                .unwrap();
            record.plan.request_generation += 1;
            record.plan.completed_generation = record.plan.request_generation;
        }
    }
    let quotas = bundle.admin_provider().read_quotas(&requests).await;
    assert!(quotas.into_iter().all(|quota| quota.is_ok()));
    assert_eq!(
        store
            .snapshot_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        records.len() + 2
    );
}

struct PeerFixture {
    provider: provider_openai::ProviderBundle,
    peer: gateway_admin::PeerUpstreamBundle,
}

impl PeerFixture {
    fn admin_provider(&self) -> Arc<dyn gateway_admin::ports::peer::PeerUpstreamService> {
        self.peer.service()
    }

    fn core_provider(&self) -> Arc<dyn gateway_core::engine::provider::Provider> {
        self.provider.core_provider()
    }

    fn take_worker_contributions(&mut self) -> Vec<WorkerContribution> {
        self.peer.take_worker_contributions()
    }
}

async fn peer_fixture(
    config: provider_openai::OpenAiConfig,
    accounts: Arc<MemoryAccountStore>,
    store: Arc<MemoryUpstreamInfoStore>,
) -> PeerFixture {
    let reader = Arc::new(gateway_core::account::UpstreamInfoReader::default());
    let provider = provider_openai::initialize(
        config,
        provider_ports_with_catalog(
            accounts.clone(),
            Arc::new(TestOAuthPending::default()),
            Arc::new(TestCatalogCache::default()),
        ),
        reader.clone(),
    )
    .await
    .unwrap();
    let registry =
        gateway_admin::ports::provider::ProviderAdminRegistry::new([provider.admin_provider()])
            .unwrap();
    let peer = gateway_admin::initialize_peer_upstream(
        accounts.clone(),
        store,
        &registry,
        Arc::new(gateway_host::peer::HttpPeerTransport::with_client_builder(
            provider_openai::build_reqwest_client_with_custom_ca,
        )),
    )
    .unwrap();
    reader.bind(&peer.scheduling_source()).unwrap();
    PeerFixture { provider, peer }
}
