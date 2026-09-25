use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_admin::{
    model::{
        Revision,
        plugins::instances::{
            PluginCapabilityBinding, PluginFailurePolicy, PluginInstance, PluginInstanceSnapshot,
            PluginPermissionGrant,
        },
    },
    ports::plugins::PluginPackageInspector,
};
use gateway_core::{
    account::{
        AccountCandidate, AccountEligibilityPolicy, AccountRuntimeSignals, AccountSelectionContext,
        AccountSelectionPolicy, AccountWeight, CredentialRevision, CredentialState,
        ProviderAccount, ProviderAccountId, QuotaState, RotationStrategy,
    },
    engine::{
        ModelRequestId,
        execution::ClientTransport,
        extensions::ExtensionCallScope,
        middleware::{
            MiddlewareAuthority, MiddlewareBody, MiddlewareContext, MiddlewareError,
            MiddlewareFrame, MiddlewareFraming, MiddlewareHeader, MiddlewareMount, MiddlewareNext,
            MiddlewareRequest, MiddlewareResponse, MiddlewareTarget,
        },
        nested::ExecutionEffects,
        policy::{ModelRouteDecision, RequestPolicyContext},
    },
    identity::ProviderKind,
    lifecycle::CancellationToken,
    operation::{GenerateRequest, Operation, OperationKind, ProtocolPayload},
    policy::ClientApiKeyId,
    routing::{AccountGroupId, ConfigRevision, PublicModelId},
    runtime::extensions::{ExtensionPreparationPort, ExtensionSetReference},
};
use gateway_plugin_runtime::{
    PackageInspector, PackageLimits, PluginRuntime, PluginRuntimeConfig, RpcLimits,
};
use gateway_plugin_sdk::{Capability, Contributions, Permission, Stage};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

use crate::support::{
    environment::{Environment, account_grant},
    store::Store,
};

struct InstanceFixture {
    id: &'static str,
    configuration: serde_json::Value,
    grants: Vec<PluginPermissionGrant>,
    bindings: Vec<PluginCapabilityBinding>,
}

const MODEL_ROUTER_CONTRIBUTION: &str = "test.example.modelRouter";
const SCHEDULER_CONTRIBUTION: &str = "test.example.scheduler";
const MIDDLEWARE_CONTRIBUTION: &str = "test.example.middleware";

fn binding(
    contribution: &str,
    stage: &str,
    order: i32,
    failure_policy: PluginFailurePolicy,
) -> PluginCapabilityBinding {
    PluginCapabilityBinding {
        contribution: contribution.into(),
        stage: stage.into(),
        order,
        failure_policy,
        client_key_ids: vec![],
        account_group_ids: vec![],
        provider_ids: vec![],
        models: vec![],
        identity_bindings: vec![],
    }
}

fn grant(permission: Permission) -> PluginPermissionGrant {
    PluginPermissionGrant {
        permission: permission.as_str().to_owned(),
    }
}

async fn setup(
    instances: Vec<InstanceFixture>,
    manifest_permissions: Vec<Permission>,
) -> (tempfile::TempDir, PluginRuntime) {
    let contributes = Contributions::from([
        crate::support::contribution(Capability::RetryPolicy, vec![Stage::Retry], vec![], vec![]),
        crate::support::contribution(
            Capability::ModelRouter,
            vec![Stage::Routing],
            vec![],
            vec![],
        ),
        crate::support::contribution(
            Capability::Scheduler,
            vec![Stage::Scheduling],
            vec![],
            vec![],
        ),
        crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Request, Stage::Attempt],
            vec!["openai".into()],
            vec!["openai".into()],
        ),
    ]);
    setup_package(
        instances,
        crate::support::package_with_contributions(
            crate::support::worker(),
            manifest_permissions,
            contributes,
        ),
    )
    .await
}

async fn setup_package(
    instances: Vec<InstanceFixture>,
    package: Arc<[u8]>,
) -> (tempfile::TempDir, PluginRuntime) {
    let cache = tempfile::tempdir().unwrap();
    let artifact = PackageInspector::new(PackageLimits::default(), "1.0.0".parse().unwrap())
        .inspect(package, None)
        .await
        .unwrap();
    let digest = artifact.metadata.sha256.clone();
    let instances = instances
        .into_iter()
        .map(|fixture| PluginInstance {
            id: fixture.id.into(),
            name: fixture.id.into(),
            artifact_sha256: digest.clone(),
            enabled: true,
            trusted_process: true,
            configuration: fixture.configuration,
            secrets: BTreeMap::new(),
            grants: fixture.grants,
            bindings: fixture.bindings,
            revision: Revision::new(1).unwrap(),
        })
        .collect();
    let store = Arc::new(Store {
        artifacts: BTreeMap::from([(digest, artifact)]),
        snapshot: std::sync::Mutex::new(PluginInstanceSnapshot {
            config_revision: Revision::new(1).unwrap(),
            instances,
        }),
    });
    let runtime = PluginRuntime::new(
        store.clone(),
        store,
        PluginRuntimeConfig {
            cache_directory: cache.path().to_owned(),
            host_version: "1.0.0".parse().unwrap(),
            package_limits: PackageLimits::default(),
            rpc_limits: RpcLimits::default(),
            restart_circuit: Default::default(),
        },
        Arc::new(gateway_host::outbound::HttpClient::new().unwrap()),
        Arc::new(gateway_host::process::ProcessSupervisor::new(
            NonZeroUsize::new(32).unwrap(),
        )),
    )
    .with_network_policy(
        gateway_host::outbound::NetworkPolicy::new(&["127.0.0.0/8".into(), "::1/128".into()])
            .unwrap(),
    );
    (cache, runtime)
}

async fn prepare(runtime: &PluginRuntime) -> ExtensionSetReference {
    ExtensionPreparationPort::prepare(runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap()
}

fn policy_context(
    runtime: &PluginRuntime,
    generation: ExtensionSetReference,
    request_id: &str,
) -> RequestPolicyContext {
    RequestPolicyContext::new(
        runtime.policy_registry().resolve(&generation).unwrap(),
        generation,
        ModelRequestId::new(request_id).unwrap(),
        ClientApiKeyId::new("key-policy").unwrap(),
        Vec::new(),
    )
}

fn operation() -> Operation {
    let context = serde_json::json!({
        "opaque_request_headers": [
            ["x-feature", "b24="],
            ["authorization", "c2VjcmV0"],
            ["session_id", "c2VjcmV0"],
            ["thread_id", "c2VjcmV0"],
            ["X_Extension!#$%&'*+-.^_`|~", "b24="],
        ]
    })
    .as_object()
    .unwrap()
    .clone();
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            serde_json::json!({"model":"public-a","input":"synthetic"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap()
        .with_context(context),
    ))
}

#[tokio::test]
async fn retry_policy_uses_ordered_delegation_and_rejects_disallowed_actions() {
    use gateway_core::{
        engine::policy::{RetryDecision, RetryFacts},
        error::ProviderErrorKind,
        upstream::UpstreamSendState,
    };
    for (reply, allowed, expected) in [
        (
            serde_json::json!({"decision":"delegate"}),
            true,
            RetryDecision::Stop,
        ),
        (
            serde_json::json!({"decision":"retry"}),
            true,
            RetryDecision::Retry,
        ),
        (
            serde_json::json!({"decision":"retry"}),
            false,
            RetryDecision::Stop,
        ),
        (
            serde_json::json!({"decision":"retry","account_id":"acct_forbidden"}),
            true,
            RetryDecision::Stop,
        ),
    ] {
        let records = tempfile::tempdir().unwrap();
        let marker = records.path().join("retry.jsonl");
        let (_cache, runtime) = setup(vec![
            InstanceFixture { id: "retry-first", configuration: serde_json::json!({"retry_decision":reply,"retry_marker":marker}), grants: vec![],
                bindings: vec![binding("test.example.retryPolicy", "retry", 0, PluginFailurePolicy::Delegate)] },
            InstanceFixture { id: "retry-second", configuration: serde_json::json!({"retry_decision":{"decision":"stop"}}), grants: vec![],
                bindings: vec![binding("test.example.retryPolicy", "retry", 1, PluginFailurePolicy::Delegate)] },
        ], vec![]).await;
        let context = policy_context(&runtime, prepare(&runtime).await, "req_retry");
        let decision = context
            .retry_decision(RetryFacts {
                attempt_index: NonZeroU32::MIN,
                provider: ProviderKind::new("openai").unwrap(),
                model: Some("gpt-test".into()),
                error_kind: ProviderErrorKind::RateLimited,
                upstream_status: Some(429),
                send_state: UpstreamSendState::NotSent,
                remaining_routing_attempts: 1,
                remaining_deadline: Duration::from_secs(3),
                retry_allowed: allowed,
            })
            .await;
        assert_eq!(decision, expected);
        let facts = marker_lines(&marker, 1).await.remove(0);
        assert_eq!(
            facts["allowed_actions"],
            if allowed {
                serde_json::json!(["stop", "retry"])
            } else {
                serde_json::json!(["stop"])
            }
        );
        assert_eq!(facts["error_kind"], "rate_limited");
        assert!(facts.get("account_id").is_none());
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn retry_policy_timeout_exhausts_the_chain_budget_and_delegates() {
    use gateway_core::{
        engine::policy::{RetryDecision, RetryFacts},
        error::ProviderErrorKind,
        upstream::UpstreamSendState,
    };
    let records = tempfile::tempdir().unwrap();
    let second_marker = records.path().join("second.jsonl");
    let (_cache, runtime) = setup(
        vec![
            InstanceFixture {
                id: "retry-delayed",
                configuration: serde_json::json!({"retry_delay_ms":500,"retry_decision":{"decision":"stop"}}),
                grants: vec![],
                bindings: vec![binding("test.example.retryPolicy", "retry", 0, PluginFailurePolicy::Delegate)],
            },
            InstanceFixture {
                id: "retry-next",
                configuration: serde_json::json!({"retry_marker":second_marker,"retry_decision":{"decision":"stop"}}),
                grants: vec![],
                bindings: vec![binding("test.example.retryPolicy", "retry", 1, PluginFailurePolicy::Delegate)],
            },
        ],
        vec![],
    ).await;
    let context = policy_context(&runtime, prepare(&runtime).await, "req_retry_timeout");
    let decision = tokio::time::timeout(
        Duration::from_secs(2),
        context.retry_decision(RetryFacts {
            attempt_index: NonZeroU32::MIN,
            provider: ProviderKind::new("openai").unwrap(),
            model: Some("gpt-test".into()),
            error_kind: ProviderErrorKind::RateLimited,
            upstream_status: Some(429),
            send_state: UpstreamSendState::NotSent,
            remaining_routing_attempts: 1,
            remaining_deadline: Duration::from_millis(100),
            retry_allowed: true,
        }),
    )
    .await
    .expect("the shared deadline bounds the whole retry chain");
    assert_eq!(decision, RetryDecision::Delegate);
    assert!(
        !second_marker.exists(),
        "later policies cannot restart an exhausted budget"
    );
    runtime.shutdown().await;
}

async fn marker_lines(path: &std::path::Path, count: usize) -> Vec<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let lines = std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<_>>();
            if lines.len() >= count {
                return lines;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("plugin marker")
}

#[tokio::test]
async fn model_router_preserves_order_and_only_projects_authorized_request_data() {
    let markers = tempfile::tempdir().unwrap();
    let marker = markers.path().join("routes.jsonl");
    let (cache, runtime) = setup(
        vec![
            InstanceFixture {
                id: "unhandled",
                configuration: serde_json::json!({
                    "route_marker":marker,
                    "route_decision":{"decision":"unhandled"},
                }),
                grants: vec![],
                bindings: vec![binding(
                    MODEL_ROUTER_CONTRIBUTION,
                    "routing",
                    1,
                    PluginFailurePolicy::Reject,
                )],
            },
            InstanceFixture {
                id: "handled",
                configuration: serde_json::json!({
                    "route_marker":marker,
                    "route_decision":{"decision":"route","provider":"openai","model":"public-b"},
                }),
                grants: vec![grant(Permission::Requests)],
                bindings: vec![binding(
                    MODEL_ROUTER_CONTRIBUTION,
                    "routing",
                    2,
                    PluginFailurePolicy::Reject,
                )],
            },
        ],
        vec![Permission::Requests],
    )
    .await;
    let generation = prepare(&runtime).await;
    let context = policy_context(&runtime, generation.clone(), "req_route");
    let decision = context
        .route_model(
            operation(),
            PublicModelId::new("public-a").unwrap(),
            BTreeSet::from([
                ProviderKind::new("openai").unwrap(),
                ProviderKind::new("example").unwrap(),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(
        decision,
        ModelRouteDecision::Route {
            provider: Some(ProviderKind::new("openai").unwrap()),
            model: Some(PublicModelId::new("public-b").unwrap()),
        }
    );
    let lines = marker_lines(&marker, 2).await;
    assert_eq!(lines[0]["body"], "");
    assert_eq!(lines[0]["request"]["headers"], serde_json::json!([]));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[1]["body"].as_str().unwrap()).unwrap(),
        serde_json::json!({"model":"public-a","input":"synthetic"})
    );
    assert_eq!(
        lines[1]["request"]["headers"],
        serde_json::json!([
            {"name":"x-feature","value_base64":"b24="},
            {"name":"x_extension!#$%&'*+-.^_`|~","value_base64":"b24="}
        ])
    );
    assert!(!lines[1].to_string().contains("secret"));
    drop(context);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn model_router_can_delegate_after_a_managed_http_side_effect() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok":true})))
        .expect(1)
        .mount(&server)
        .await;
    let (cache, runtime) = setup(
        vec![
            InstanceFixture {
                id: "http-then-fault",
                configuration: serde_json::json!({
                    "route_http_url":server.uri(),
                    "route_fault":true,
                }),
                grants: vec![grant(Permission::Network)],
                bindings: vec![binding(
                    MODEL_ROUTER_CONTRIBUTION,
                    "routing",
                    1,
                    PluginFailurePolicy::Delegate,
                )],
            },
            InstanceFixture {
                id: "fallback",
                configuration: serde_json::json!({
                    "route_decision":{"decision":"unhandled"},
                }),
                grants: vec![],
                bindings: vec![binding(
                    MODEL_ROUTER_CONTRIBUTION,
                    "routing",
                    2,
                    PluginFailurePolicy::Reject,
                )],
            },
        ],
        vec![Permission::Network],
    )
    .await;
    let generation = prepare(&runtime).await;
    let context = policy_context(&runtime, generation.clone(), "req_route_http");
    assert_eq!(
        context
            .route_model(
                operation(),
                PublicModelId::new("public-a").unwrap(),
                BTreeSet::from([ProviderKind::new("openai").unwrap()]),
            )
            .await
            .unwrap(),
        ModelRouteDecision::Unhandled
    );
    drop(context);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

fn candidate(id: &str, weight: u16) -> AccountCandidate {
    AccountCandidate {
        account: ProviderAccount::new(
            ProviderAccountId::new(id).unwrap(),
            ProviderKind::new("example").unwrap(),
            id.into(),
            None,
            "test".into(),
            CredentialRevision::new(1).unwrap(),
            Some(SystemTime::now() + Duration::from_secs(60)),
        )
        .with_account_facts(
            true,
            CredentialState::Ready,
            QuotaState::unknown(),
            None,
            None,
        )
        .with_scheduling(None, AccountWeight::new(weight).unwrap()),
        signals: AccountRuntimeSignals {
            in_flight: 0,
            last_started_at: None,
            quota_reset_at: None,
            quota_remaining_rank: None,
            cooldown: None,
            failure_rate_basis_points: None,
            first_output_latency_ms: None,
        },
    }
}

fn selection_context() -> AccountSelectionContext {
    AccountSelectionContext {
        policy: AccountSelectionPolicy::new(
            RotationStrategy::RoundRobin,
            NonZeroU32::new(4).unwrap(),
            Duration::ZERO,
        ),
        now: SystemTime::now(),
        excluded_accounts: BTreeSet::new(),
        preferred_account: None,
        preferred_account_overrides_weight: true,
        round_robin_cursor: 0,
        eligibility: AccountEligibilityPolicy::Enforce,
        account_scope: None,
    }
}

#[tokio::test]
async fn scheduler_cannot_pick_outside_the_host_projected_priority_tier() {
    let (cache, runtime) = setup(
        vec![InstanceFixture {
            id: "scheduler",
            configuration: serde_json::json!({"schedule_pick_index":0}),
            grants: vec![],
            bindings: vec![binding(
                SCHEDULER_CONTRIBUTION,
                "scheduling",
                0,
                PluginFailurePolicy::Reject,
            )],
        }],
        vec![],
    )
    .await;
    let generation = prepare(&runtime).await;
    let context = policy_context(&runtime, generation.clone(), "req_schedule");
    let candidates = [candidate("acct_high", 100), candidate("acct_low", 10)];
    let selection = selection_context();
    let selected = context
        .select_account(
            NonZeroU32::MIN,
            &ProviderKind::new("example").unwrap(),
            Some("upstream-a"),
            &candidates,
            &selection,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(selected.candidate().account.id().as_str(), "acct_high");
    drop(context);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

struct OneFrameBody {
    frame: Option<MiddlewareFrame>,
    reads: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
}

impl MiddlewareBody for OneFrameBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.frame.take())
        })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.closes.fetch_add(1, Ordering::Relaxed);
        })
    }
}

struct Downstream {
    calls: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    error: bool,
}

impl MiddlewareNext for Downstream {
    fn run(
        self: Box<Self>,
        request: MiddlewareRequest,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert_eq!(request.protocol(), "openai");
            assert_eq!(request.body(), &Bytes::from_static(br#"{"input":"hello"}"#));
            assert!(request.headers().iter().any(|header| {
                header.name() == "authorization" && header.value().as_ref() == b"Bearer private"
            }));
            assert!(
                request
                    .headers()
                    .iter()
                    .any(|header| header.name() == "x_team" && header.value().as_ref() == b"team")
            );
            let (status, framing, body) = if self.error {
                (502, MiddlewareFraming::RawBytes, b"\0upstream".as_slice())
            } else {
                (
                    200,
                    MiddlewareFraming::JsonDocument,
                    br#"{"downstream":true}"#.as_slice(),
                )
            };
            Ok(MiddlewareResponse::new(
                "openai".into(),
                status,
                vec![
                    MiddlewareHeader::new("content-type", Bytes::from_static(b"application/json")),
                    MiddlewareHeader::new("x-upstream-marker", Bytes::from_static(b"kept")),
                ],
                Box::new(OneFrameBody {
                    reads: Arc::clone(&self.reads),
                    frame: Some(MiddlewareFrame::new(
                        Bytes::copy_from_slice(body),
                        framing,
                        true,
                    )),
                    closes: Arc::clone(&self.closes),
                }),
            ))
        })
    }
}

fn middleware_context(transport: ClientTransport) -> MiddlewareContext {
    MiddlewareContext::new(
        MiddlewareTarget {
            request_id: ModelRequestId::new("req_middleware").unwrap(),
            mount: MiddlewareMount::Request,
            attempt_index: None,
            operation: Some(OperationKind::Generate),
            endpoint: "/v1/responses".into(),
            transport,
            provider: None,
            model: Some("public-a".into()),
            account_id: None,
        },
        MiddlewareAuthority {
            client_key_id: ClientApiKeyId::new("key-middleware").unwrap(),
            account_group_ids: Arc::<[AccountGroupId]>::from([]),
            cancellation: CancellationToken::new(),
            deadline: SystemTime::now() + Duration::from_secs(5),
            extension_scope: ExtensionCallScope::default(),
            execution_effects: None,
        },
    )
}

fn attempt_middleware_context(effects: Arc<ExecutionEffects>) -> MiddlewareContext {
    MiddlewareContext::new(
        MiddlewareTarget {
            request_id: ModelRequestId::new("req_attempt_http").unwrap(),
            mount: MiddlewareMount::Attempt,
            attempt_index: Some(NonZeroU32::MIN),
            operation: Some(OperationKind::Generate),
            endpoint: "/v1/responses".into(),
            transport: ClientTransport::HttpJson,
            provider: Some(ProviderKind::new("openai").unwrap()),
            model: Some("public-a".into()),
            account_id: Some(ProviderAccountId::new("acct_openai").unwrap()),
        },
        MiddlewareAuthority {
            client_key_id: ClientApiKeyId::new("key-middleware").unwrap(),
            account_group_ids: Arc::<[AccountGroupId]>::from([]),
            cancellation: CancellationToken::new(),
            deadline: SystemTime::now() + Duration::from_secs(5),
            extension_scope: ExtensionCallScope::default(),
            execution_effects: Some(effects),
        },
    )
}

fn middleware_headers() -> Vec<MiddlewareHeader> {
    vec![
        MiddlewareHeader::new("authorization", Bytes::from_static(b"Bearer private")),
        MiddlewareHeader::new("connection", Bytes::from_static(b"x-hop")),
        MiddlewareHeader::new("x-hop", Bytes::from_static(b"hidden")),
        MiddlewareHeader::new("x_team", Bytes::from_static(b"team")),
    ]
}

#[tokio::test]
async fn attempt_middleware_can_delegate_after_a_managed_http_side_effect() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let (cache, runtime) = setup(
        vec![InstanceFixture {
            id: "attempt-http",
            configuration: serde_json::json!({
                "middleware_http_url":server.uri(),
                "middleware_fault_before_next":true,
            }),
            grants: vec![grant(Permission::Network)],
            bindings: vec![binding(
                MIDDLEWARE_CONTRIBUTION,
                "attempt",
                0,
                PluginFailurePolicy::Delegate,
            )],
        }],
        vec![Permission::Network],
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime
        .middleware_registry()
        .resolve(&generation)
        .expect("middleware plan");
    let calls = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let effects = Arc::new(ExecutionEffects::default());
    let response = plan
        .handle(
            attempt_middleware_context(effects),
            MiddlewareRequest::new(
                "openai",
                middleware_headers(),
                Bytes::from_static(br#"{"input":"hello"}"#),
            ),
            Box::new(Downstream {
                calls: Arc::clone(&calls),
                closes: Arc::clone(&closes),
                reads: Arc::default(),
                error: false,
            }),
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let (_, status, _, body, _) = response.into_parts();
    assert_eq!(status, 200);
    body.close().await;
    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn middleware_next_and_lazy_body_mapping_use_one_real_rpc_call() {
    let markers = tempfile::tempdir().unwrap();
    let marker = markers.path().join("middleware.jsonl");
    let permissions = vec![Permission::Requests];
    let (cache, runtime) = setup(
        vec![InstanceFixture {
            id: "middleware",
            configuration: serde_json::json!({
                "middleware_marker":marker,
                "middleware_map_body":true,
            }),
            grants: permissions.iter().copied().map(grant).collect(),
            bindings: vec![binding(
                MIDDLEWARE_CONTRIBUTION,
                "request",
                0,
                PluginFailurePolicy::Reject,
            )],
        }],
        permissions,
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime
        .middleware_registry()
        .resolve(&generation)
        .expect("middleware plan");
    let calls = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let context = middleware_context(ClientTransport::HttpJson);
    let response = plan
        .handle(
            context.clone(),
            MiddlewareRequest::new(
                "openai",
                middleware_headers(),
                Bytes::from_static(br#"{"input":"hello"}"#),
            ),
            Box::new(Downstream {
                calls: Arc::clone(&calls),
                closes: Arc::clone(&closes),
                reads: Arc::default(),
                error: false,
            }),
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let (protocol, status, headers, mut body, envelope) = response.into_parts();
    assert_eq!(protocol, "openai");
    assert_eq!(status, 200);
    assert!(headers.iter().any(|header| {
        header.name() == "content-type" && header.value().as_ref() == b"application/json"
    }));
    assert!(envelope.is_none());
    let frame = body.next_frame().await.unwrap().unwrap();
    assert_eq!(frame.framing(), MiddlewareFraming::JsonDocument);
    assert!(frame.terminal());
    assert!(frame.transformed());
    assert_eq!(
        frame.into_bytes(),
        Bytes::from_static(b"{\"downstream\":true} ")
    );
    assert!(body.next_frame().await.unwrap().is_none());
    let first_marker = &marker_lines(&marker, 1).await[0];
    assert_eq!(first_marker["body"], "{\"input\":\"hello\"}");
    assert_eq!(
        first_marker["headers"],
        serde_json::json!([{
            "name":"x_team",
            "value":[116,101,97,109],
        }])
    );
    body.close().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while closes.load(Ordering::Relaxed) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let response = plan
        .handle(
            context,
            MiddlewareRequest::new(
                "openai",
                middleware_headers(),
                Bytes::from_static(br#"{"input":"hello"}"#),
            ),
            Box::new(Downstream {
                calls: Arc::clone(&calls),
                closes: Arc::clone(&closes),
                reads: Arc::default(),
                error: false,
            }),
        )
        .await
        .unwrap();
    let (_, _, _, body, _) = response.into_parts();
    body.close().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while closes.load(Ordering::Relaxed) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    let response = plan
        .handle(
            middleware_context(ClientTransport::HttpSse),
            MiddlewareRequest::new(
                "openai",
                middleware_headers(),
                Bytes::from_static(br#"{"input":"hello"}"#),
            ),
            Box::new(Downstream {
                calls: Arc::clone(&calls),
                closes: Arc::clone(&closes),
                reads: Arc::default(),
                error: true,
            }),
        )
        .await
        .unwrap();
    let (_, status, _, mut body, _) = response.into_parts();
    assert_eq!(status, 502);
    let frame = body.next_frame().await.unwrap().unwrap();
    assert_eq!(frame.framing(), MiddlewareFraming::RawBytes);
    assert!(frame.transformed());
    assert_eq!(frame.into_bytes(), Bytes::from_static(b"\0upstream "));
    body.close().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while closes.load(Ordering::Relaxed) != 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn middleware_classifies_real_rpc_output_after_a_terminal_frame() {
    for (id, extra_chunk, terminal_error) in [
        ("middleware-terminal-chunk", true, false),
        ("middleware-terminal-error", false, true),
    ] {
        let permissions = vec![Permission::Requests];
        let (cache, runtime) = setup(
            vec![InstanceFixture {
                id,
                configuration: serde_json::json!({
                    "middleware_map_body": true,
                    "middleware_chunk_after_terminal": extra_chunk,
                    "middleware_error_after_terminal": terminal_error,
                }),
                grants: permissions.iter().copied().map(grant).collect(),
                bindings: vec![binding(
                    MIDDLEWARE_CONTRIBUTION,
                    "request",
                    0,
                    PluginFailurePolicy::Reject,
                )],
            }],
            permissions,
        )
        .await;
        let generation = prepare(&runtime).await;
        let plan = runtime.middleware_registry().resolve(&generation).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        let response = plan
            .handle(
                middleware_context(ClientTransport::HttpJson),
                MiddlewareRequest::new(
                    "openai",
                    middleware_headers(),
                    Bytes::from_static(br#"{"input":"hello"}"#),
                ),
                Box::new(Downstream {
                    calls: Arc::clone(&calls),
                    closes: Arc::clone(&closes),
                    reads: Arc::default(),
                    error: false,
                }),
            )
            .await
            .unwrap();
        let (_, _, _, mut body, _) = response.into_parts();
        let error = body
            .next_frame()
            .await
            .expect_err("output after the terminal frame must fail");
        let expected = if terminal_error {
            matches!(&error, MiddlewareError::Fault)
        } else {
            matches!(&error, MiddlewareError::InvalidState)
        };
        assert!(
            expected,
            "unexpected terminal stream error for {id}: {error:?}"
        );
        body.close().await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        drop(plan);
        drop(generation);
        super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn sdk_middleware_entry_registers_and_maps_a_real_process_response() {
    let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
    let permissions = vec![Permission::Requests];
    let package = crate::support::package_with_contributions(
        &worker,
        permissions.clone(),
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Request],
            vec!["openai".into()],
            vec!["openai".into()],
        )]),
    );
    let (cache, runtime) = setup_package(
        vec![InstanceFixture {
            id: "sdk-middleware",
            configuration: serde_json::json!({}),
            grants: permissions.into_iter().map(grant).collect(),
            bindings: vec![binding(
                MIDDLEWARE_CONTRIBUTION,
                "request",
                0,
                PluginFailurePolicy::Reject,
            )],
        }],
        package,
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime.middleware_registry().resolve(&generation).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let response = plan
        .handle(
            middleware_context(ClientTransport::HttpJson),
            MiddlewareRequest::new(
                "openai",
                middleware_headers(),
                Bytes::from_static(br#"{"input":"hello"}"#),
            ),
            Box::new(Downstream {
                calls: Arc::clone(&calls),
                closes: Arc::clone(&closes),
                reads: Arc::clone(&reads),
                error: false,
            }),
        )
        .await
        .unwrap();
    let (_, status, headers, mut body, _) = response.into_parts();
    assert_eq!(status, 200);
    assert!(
        headers.iter().any(|header| {
            header.name() == "x-sdk-plugin" && header.value().as_ref() == b"active"
        })
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while reads.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // 模拟慢消费者：SDK 可以继续发起读回调，但宿主不得预读第二个源。
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    let frame = body.next_frame().await.unwrap().unwrap();
    assert!(frame.terminal());
    assert!(frame.transformed());
    assert_eq!(
        frame.into_bytes(),
        Bytes::from_static(b"{\"downstream\":true} ")
    );
    assert!(body.next_frame().await.unwrap().is_none());
    assert_eq!(reads.load(Ordering::Relaxed), 2);
    body.close().await;
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    tokio::time::timeout(Duration::from_secs(1), async {
        while closes.load(Ordering::Relaxed) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // 尚未消费源帧就关闭，等待源归还的回调必须一起取消并释放下游正文。
    reads.store(0, Ordering::Relaxed);
    let response = plan
        .handle(
            middleware_context(ClientTransport::HttpJson),
            MiddlewareRequest::new(
                "openai",
                middleware_headers(),
                Bytes::from_static(br#"{"input":"hello"}"#),
            ),
            Box::new(Downstream {
                calls: Arc::clone(&calls),
                reads: Arc::clone(&reads),
                closes: Arc::clone(&closes),
                error: false,
            }),
        )
        .await
        .unwrap();
    let (_, _, _, body, _) = response.into_parts();
    tokio::time::timeout(Duration::from_secs(5), async {
        while reads.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    body.close().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while closes.load(Ordering::Relaxed) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn core_execution_resolves_the_plugin_runtime_middleware_plan() {
    let Some(environment) = Environment::create().await else {
        return;
    };
    let marker = environment.directory.path().join("core-middleware.jsonl");
    environment
        .install_plugin(
            serde_json::json!({"middleware_marker": marker}),
            vec![account_grant("accounts")],
        )
        .await;
    let key_secret = "sk-core-middleware-fixture";
    environment
        .client_key("key_core_middleware", key_secret)
        .await;

    let (runtime, core) = environment.runtime().await;
    let execution = core.execution_service();
    let client = execution.authenticate(key_secret).unwrap();
    let prepared = execution.prepare_execution(client).await.unwrap();
    assert!(execution.middleware_plan(&prepared).is_some());

    environment.release_plugin_accounts(&runtime);
    drop(core);
    drop(runtime);
    environment.close().await;
}

#[tokio::test]
async fn instances_without_data_plane_bindings_publish_neither_plan() {
    let (cache, runtime) = setup(
        vec![InstanceFixture {
            id: "unbound",
            configuration: serde_json::json!({}),
            grants: vec![],
            bindings: vec![],
        }],
        vec![],
    )
    .await;
    let generation = prepare(&runtime).await;
    assert!(runtime.policy_registry().resolve(&generation).is_none());
    assert!(runtime.middleware_registry().resolve(&generation).is_none());
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

struct TransparencyNext {
    expected: MiddlewareRequest,
    headers: Vec<MiddlewareHeader>,
    frames: VecDeque<MiddlewareFrame>,
}

struct CapabilitiesNext(Arc<AtomicUsize>);

impl MiddlewareNext for CapabilitiesNext {
    fn run(
        self: Box<Self>,
        request: MiddlewareRequest,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            let body = serde_json::from_slice(request.body()).unwrap();
            let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object("openai", body).unwrap(),
            ));
            let operation = request.apply_capabilities(operation)?;
            assert!(
                operation
                    .original_capability_requirements()
                    .features()
                    .contains(&gateway_core::operation::Feature::JsonSchema)
            );
            assert!(
                !operation
                    .capability_requirements()
                    .features()
                    .contains(&gateway_core::operation::Feature::JsonSchema)
            );
            Ok(MiddlewareResponse::new(
                "openai".into(),
                200,
                vec![],
                Box::new(TransparencyBody(VecDeque::from([MiddlewareFrame::new(
                    Bytes::from_static(br#"{"output":[]}"#),
                    MiddlewareFraming::JsonDocument,
                    true,
                )]))),
            ))
        })
    }
}

#[tokio::test]
async fn sdk_capability_declarations_require_v2_authorization_and_request_stage() {
    for (version, authorized, attempt) in [
        (2, true, false),
        (1, true, false),
        (2, false, false),
        (2, true, true),
    ] {
        let stage = if attempt {
            Stage::Attempt
        } else {
            Stage::Request
        };
        let (capability, mut contribution) = crate::support::contribution(
            Capability::Middleware,
            vec![stage],
            vec!["openai".into()],
            vec!["openai".into()],
        );
        contribution.version = version;
        let permissions = if authorized {
            vec![Permission::Requests]
        } else {
            vec![]
        };
        let package = crate::support::package_with_contributions(
            &std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap(),
            permissions.clone(),
            Contributions::from([(capability, contribution)]),
        );
        let (_cache, runtime) = setup_package(vec![InstanceFixture {
            id: "capability-declaration",
            configuration: serde_json::json!({"mode":"declare","middleware_version":version,"attempt":attempt,
                "body":{"model":"gpt-test","input":"return JSON matching the original schema"},
                "capabilities":{"handled":["json_schema"],"required":[]}}),
            grants: permissions.into_iter().map(grant).collect(),
            bindings: vec![binding(MIDDLEWARE_CONTRIBUTION, if attempt { "attempt" } else { "request" }, 0, PluginFailurePolicy::Reject)],
        }], package).await;
        let generation = prepare(&runtime).await;
        let plan = runtime.middleware_registry().resolve(&generation).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let context = if attempt {
            attempt_middleware_context(Arc::default())
        } else {
            middleware_context(ClientTransport::HttpJson)
        };
        let result = plan.handle(context, MiddlewareRequest::new("openai", vec![], Bytes::from_static(
            br#"{"model":"gpt-test","input":"answer","text":{"format":{"type":"json_schema","schema":{"type":"object"}}}}"#,
        )), Box::new(CapabilitiesNext(calls.clone()))).await;
        if version == 2 && authorized && !attempt {
            let mut body = result.unwrap().into_parts().3;
            assert!(body.next_frame().await.unwrap().unwrap().terminal());
            assert!(body.next_frame().await.unwrap().is_none());
            body.close().await;
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        } else {
            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        runtime.shutdown().await;
    }
}

impl MiddlewareNext for TransparencyNext {
    fn run(
        self: Box<Self>,
        request: MiddlewareRequest,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        Box::pin(async move {
            assert_eq!(request.protocol(), self.expected.protocol());
            assert_eq!(request.body(), self.expected.body());
            assert_eq!(request.headers(), self.expected.headers());
            Ok(MiddlewareResponse::new(
                "openai".into(),
                200,
                self.headers,
                Box::new(TransparencyBody(self.frames)),
            ))
        })
    }
}

struct TransparencyBody(VecDeque<MiddlewareFrame>);

impl MiddlewareBody for TransparencyBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move { Ok(self.0.pop_front()) })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

async fn assert_sdk_transparency(mode: &'static str, authorized: bool) {
    let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
    let permissions = if authorized {
        vec![Permission::Requests]
    } else {
        vec![]
    };
    let package = crate::support::package_with_contributions(
        &worker,
        permissions.clone(),
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Request],
            vec!["openai".into()],
            vec!["openai".into()],
        )]),
    );
    let (cache, runtime) = setup_package(
        vec![InstanceFixture {
            id: "transparency",
            configuration: serde_json::json!({"mode": mode}),
            grants: permissions.into_iter().map(grant).collect(),
            bindings: vec![binding(
                MIDDLEWARE_CONTRIBUTION,
                "request",
                0,
                PluginFailurePolicy::Reject,
            )],
        }],
        package,
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime.middleware_registry().resolve(&generation).unwrap();
    let mut headers = middleware_headers();
    headers.extend([
        MiddlewareHeader::new("X-Repeated", Bytes::from_static(b"first")),
        MiddlewareHeader::new("X-Repeated", Bytes::from_static(b"second")),
        MiddlewareHeader::new("x-binary", Bytes::from_static(b"\x80\xff")),
    ]);
    let response_headers = vec![
        MiddlewareHeader::new("x-repeated", Bytes::from_static(b"first")),
        MiddlewareHeader::new("x-repeated", Bytes::from_static(b"second")),
        MiddlewareHeader::new("set-cookie", Bytes::from_static(b"test-hidden=kept")),
        MiddlewareHeader::new("x-binary", Bytes::from_static(b"\x80\xff")),
    ];
    let json = Bytes::from_static(
        br#" { "z": 18446744073709551615, "unknown": { "x": 1e2 }, "a": "\u0061" } "#,
    );
    let cases = [
        (ClientTransport::HttpJson, MiddlewareFraming::JsonDocument, vec![json.clone()]),
        (ClientTransport::HttpSse, MiddlewareFraming::SseEvent, vec![
            Bytes::from_static(b": keep-alive\r\nevent: response.created\r\ndata: { \"z\": 1, \"a\": 2 }\r\n\r\n"),
            Bytes::from_static(b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n"),
        ]),
    ];
    for (transport, framing, payloads) in cases {
        for request_body in [json.clone(), Bytes::new()] {
            let request = MiddlewareRequest::new("openai", headers.clone(), request_body.clone());
            let mut expected_headers = headers.clone();
            let mut expected_response_headers = response_headers.clone();
            if mode == "headers" {
                expected_headers.push(MiddlewareHeader::new(
                    "x-sdk-request",
                    Bytes::from_static(b"active"),
                ));
                expected_response_headers.push(MiddlewareHeader::new(
                    "x-sdk-response",
                    Bytes::from_static(b"active"),
                ));
            }
            let next = TransparencyNext {
                expected: MiddlewareRequest::new("openai", expected_headers, request_body),
                headers: response_headers.clone(),
                frames: payloads
                    .iter()
                    .enumerate()
                    .map(|(index, payload)| {
                        MiddlewareFrame::new(payload.clone(), framing, index + 1 == payloads.len())
                    })
                    .collect(),
            };
            let response = plan
                .handle(middleware_context(transport), request, Box::new(next))
                .await
                .unwrap();
            let (protocol, status, actual_headers, mut body, _) = response.into_parts();
            assert_eq!(protocol, "openai");
            assert_eq!(status, 200);
            assert_eq!(actual_headers, expected_response_headers);
            for (index, expected) in payloads.iter().enumerate() {
                let frame = body.next_frame().await.unwrap().unwrap();
                assert!(
                    !frame.transformed(),
                    "read-only inspection must retain provenance"
                );
                assert_eq!(frame.terminal(), index + 1 == payloads.len());
                assert_eq!(frame.into_bytes(), *expected);
            }
            assert!(body.next_frame().await.unwrap().is_none());
            body.close().await;
        }
    }
    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn sdk_passthrough_preserves_original_bytes_and_hidden_headers() {
    assert_sdk_transparency("passthrough", true).await;
    assert_sdk_transparency("passthrough", false).await;
}

#[tokio::test]
async fn sdk_read_only_inspection_preserves_original_bytes_and_event_order() {
    assert_sdk_transparency("inspect", true).await;
}

#[tokio::test]
async fn sdk_header_mutations_preserve_original_body_and_other_headers() {
    assert_sdk_transparency("headers", true).await;
}

#[tokio::test]
async fn a_failed_plugin_rejects_only_its_bound_models_and_preserves_delegate_policy() {
    for failure_policy in [PluginFailurePolicy::Reject, PluginFailurePolicy::Delegate] {
        let mut mount = binding(
            MODEL_ROUTER_CONTRIBUTION,
            "routing",
            0,
            failure_policy.clone(),
        );
        mount.models = vec!["plugin-required".into()];
        let (cache, runtime) = setup(
            vec![InstanceFixture {
                id: "failed-router",
                configuration: serde_json::json!({"startup":"fail"}),
                grants: vec![],
                bindings: vec![mount],
            }],
            vec![],
        )
        .await;
        let generation = prepare(&runtime).await;
        let context = policy_context(&runtime, generation.clone(), "req_isolated_failure");
        let unrelated = context
            .route_model(
                operation(),
                PublicModelId::new("unrelated-model").unwrap(),
                BTreeSet::new(),
            )
            .await;
        assert_eq!(unrelated.unwrap(), ModelRouteDecision::Unhandled);
        let required = context
            .route_model(
                operation(),
                PublicModelId::new("plugin-required").unwrap(),
                BTreeSet::new(),
            )
            .await;
        if failure_policy == PluginFailurePolicy::Reject {
            assert!(required.is_err());
        } else {
            assert_eq!(required.unwrap(), ModelRouteDecision::Unhandled);
        }
        drop(context);
        drop(generation);
        super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn a_failed_middleware_preserves_scope_and_the_configured_failure_policy() {
    for (model, failure_policy, expected_calls) in [
        ("unrelated", PluginFailurePolicy::Reject, 1),
        ("public-a", PluginFailurePolicy::Reject, 0),
        ("public-a", PluginFailurePolicy::Delegate, 1),
    ] {
        let mut mount = binding(MIDDLEWARE_CONTRIBUTION, "request", 0, failure_policy);
        mount.models = vec![model.into()];
        let (cache, runtime) = setup(
            vec![InstanceFixture {
                id: "failed-middleware",
                configuration: serde_json::json!({"startup":"fail"}),
                grants: vec![],
                bindings: vec![mount],
            }],
            vec![],
        )
        .await;
        let generation = prepare(&runtime).await;
        let plan = runtime.middleware_registry().resolve(&generation).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let response = plan
            .handle(
                middleware_context(ClientTransport::HttpJson),
                MiddlewareRequest::new(
                    "openai",
                    middleware_headers(),
                    Bytes::from_static(br#"{"input":"hello"}"#),
                ),
                Box::new(Downstream {
                    calls: calls.clone(),
                    reads: Arc::default(),
                    closes: Arc::default(),
                    error: false,
                }),
            )
            .await;
        assert_eq!(response.is_ok(), expected_calls == 1);
        assert_eq!(calls.load(Ordering::Relaxed), expected_calls);
        drop(response);
        drop(plan);
        drop(generation);
        super::wait_until_empty(cache.path()).await;
    }
}

struct BodyCheckingNext {
    calls: Arc<AtomicUsize>,
    expected: Bytes,
}

impl MiddlewareNext for BodyCheckingNext {
    fn run(
        self: Box<Self>,
        request: MiddlewareRequest,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        Box::pin(async move {
            assert!(
                request.body() == &self.expected,
                "request body changed unexpectedly"
            );
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(MiddlewareResponse::new(
                "openai".to_owned(),
                200,
                vec![],
                Box::new(OneFrameBody {
                    reads: Arc::default(),
                    closes: Arc::default(),
                    frame: Some(MiddlewareFrame::new(
                        Bytes::from_static(b"{}"),
                        MiddlewareFraming::JsonDocument,
                        true,
                    )),
                }),
            ))
        })
    }
}

#[tokio::test]
async fn middleware_preserves_large_body_and_following_requests() {
    let (cache, runtime) = setup(
        vec![InstanceFixture {
            id: "large-body",
            configuration: serde_json::json!({}),
            grants: vec![grant(Permission::Requests)],
            bindings: vec![binding(
                MIDDLEWARE_CONTRIBUTION,
                "request",
                0,
                PluginFailurePolicy::Reject,
            )],
        }],
        vec![Permission::Requests],
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime.middleware_registry().resolve(&generation).unwrap();
    for length in [4 * 1024 * 1024 + 17, 32] {
        let payload = Bytes::from(
            (0..length)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let response = plan
            .handle(
                middleware_context(ClientTransport::HttpSse),
                MiddlewareRequest::new("openai", vec![], payload.clone()),
                Box::new(BodyCheckingNext {
                    calls: Arc::clone(&calls),
                    expected: payload,
                }),
            )
            .await
            .unwrap();
        let (_, status, _, mut body, _) = response.into_parts();
        assert_eq!(status, 200);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(body.next_frame().await.unwrap().is_some());
        assert!(body.next_frame().await.unwrap().is_none());
        body.close().await;
    }
    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}
