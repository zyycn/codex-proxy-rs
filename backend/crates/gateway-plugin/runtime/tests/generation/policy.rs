use std::{
    collections::{BTreeMap, BTreeSet},
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
        provider::ProviderRegistry,
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
            continuation_drain: Default::default(),
            restart_circuit: Default::default(),
        },
        ProviderRegistry::default(),
        gateway_admin::ports::provider::ProviderAdminRegistry::new([]).unwrap(),
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
        serde_json::json!([{"name":"x-feature","value_base64":"b24="}])
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
        .install_provider(
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
