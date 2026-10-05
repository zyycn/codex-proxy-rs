//! 验证插件观察计划的范围、顺序、去重与有界异步通知

use std::{collections::BTreeMap, num::NonZeroU32, sync::Arc, time::Duration};

use gateway_admin::{
    model::{
        Revision,
        plugins::instances::{
            PluginCapabilityBinding, PluginFailurePolicy, PluginInstance, PluginInstanceSnapshot,
        },
    },
    ports::plugins::PluginPackageInspector,
};
use gateway_core::{
    account::ProviderAccountId,
    engine::{
        ModelRequestId, ModelRequestTimings,
        observation::{
            RequestObservation, RequestObservationOutcome, RequestObservationScope,
            WebSocketResponseAttempt, WebSocketResponseObservation,
        },
    },
    error::GatewayErrorKind,
    event::ProtocolWireEvent,
    identity::ProviderKind,
    metering::{ProviderReportedCost, Usage},
    operation::OperationKind,
    policy::ClientApiKeyId,
    routing::{AccountGroupId, ConfigRevision, PublicModelId},
    upstream::UpstreamSendState,
};
use gateway_plugin_runtime::{
    PackageInspector, PackageLimits, PluginRuntime, PluginRuntimeConfig, RpcLimits,
};
use gateway_plugin_sdk::{Capability, Contributions, Stage, call::observation::EventKind};

use crate::support::store::Store;

fn binding(event: EventKind, order: i32) -> PluginCapabilityBinding {
    PluginCapabilityBinding {
        contribution: "test.example.observer".into(),
        stage: "observation".into(),
        order,
        failure_policy: PluginFailurePolicy::Observe,
        client_key_ids: vec![],
        account_group_ids: vec![],
        provider_ids: vec!["openai".into()],
        models: vec!["gpt-observed".into()],
        event: Some(
            serde_json::to_value(event)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
        ),
        identity_bindings: vec![],
    }
}

struct InstanceFixture {
    id: String,
    configuration: serde_json::Value,

    bindings: Vec<PluginCapabilityBinding>,
}

async fn setup(
    instances: Vec<(&str, serde_json::Value, Vec<PluginCapabilityBinding>)>,
    rpc_limits: RpcLimits,
) -> (tempfile::TempDir, Arc<Store>, PluginRuntime) {
    setup_instances(
        instances
            .into_iter()
            .map(|(id, configuration, bindings)| InstanceFixture {
                id: id.to_owned(),
                configuration,

                bindings,
            })
            .collect(),
        rpc_limits,
    )
    .await
}

async fn setup_instances(
    instances: Vec<InstanceFixture>,

    rpc_limits: RpcLimits,
) -> (tempfile::TempDir, Arc<Store>, PluginRuntime) {
    let cache = tempfile::tempdir().unwrap();
    let contributes = Contributions::from([crate::support::contribution(
        Capability::Observer,
        vec![Stage::Observation],
        vec![],
        vec![],
    )]);
    let artifact = PackageInspector::new(PackageLimits::default(), "1.0.0".parse().unwrap())
        .inspect(
            crate::support::package_with_contributions(crate::support::worker(), contributes),
            None,
        )
        .await
        .unwrap();
    let digest = artifact.metadata.sha256.clone();
    let instances = instances
        .into_iter()
        .map(|fixture| PluginInstance {
            id: fixture.id.clone(),
            name: fixture.id,
            artifact_sha256: digest.clone(),
            enabled: true,
            trusted_process: true,
            configuration: fixture.configuration,
            secrets: BTreeMap::new(),

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
        store.clone(),
        PluginRuntimeConfig {
            cache_directory: cache.path().to_owned(),
            host_version: "1.0.0".parse().unwrap(),
            package_limits: PackageLimits::default(),
            rpc_limits,
            restart_circuit: Default::default(),
        },
        Arc::new(gateway_host::outbound::HttpClient::new().unwrap()),
        Arc::new(gateway_host::process::ProcessSupervisor::new(
            std::num::NonZeroUsize::new(16).unwrap(),
        )),
    );
    (cache, store, runtime)
}

fn observation(id: &str, outcome: RequestObservationOutcome) -> RequestObservation {
    observation_with_scope(
        id,
        outcome,
        "client-key-observed",
        vec![AccountGroupId::new("grp_11111111111111111111111111111111").unwrap()],
    )
}

fn observation_with_scope(
    id: &str,
    outcome: RequestObservationOutcome,
    client_key_id: &str,
    account_group_ids: Vec<AccountGroupId>,
) -> RequestObservation {
    let send_state = if outcome == RequestObservationOutcome::Rejected {
        UpstreamSendState::NotSent
    } else {
        UpstreamSendState::Sent
    };
    RequestObservation::new(
        ModelRequestId::new(id).unwrap(),
        ConfigRevision::new(1).unwrap(),
        RequestObservationScope::new(
            ClientApiKeyId::new(client_key_id).unwrap(),
            account_group_ids,
        ),
        OperationKind::Generate,
        outcome,
        send_state,
        std::time::SystemTime::now(),
    )
    .with_requested_model(PublicModelId::new("gpt-observed").unwrap())
    .with_usage(Usage {
        input_tokens: Some(11),
        output_tokens: Some(7),
        total_tokens: Some(18),
        ..Usage::default()
    })
}

fn websocket_observation(
    id: &str,
    sequence: u64,
    body: serde_json::Value,
) -> WebSocketResponseObservation {
    WebSocketResponseObservation::new(
        ModelRequestId::new(id).unwrap(),
        ConfigRevision::new(1).unwrap(),
        RequestObservationScope::new(
            ClientApiKeyId::new("client-key-observed").unwrap(),
            vec![AccountGroupId::new("grp_11111111111111111111111111111111").unwrap()],
        ),
        OperationKind::Generate,
        WebSocketResponseAttempt::new(
            ProviderKind::new("openai").unwrap(),
            ProviderAccountId::new("acct_observed").unwrap(),
            NonZeroU32::new(2).unwrap(),
        ),
        sequence,
        ProtocolWireEvent::json("openai", Some("response.output_text.delta".into()), body).unwrap(),
    )
    .with_requested_model(PublicModelId::new("gpt-observed").unwrap())
}

async fn wait_for_lines(path: &std::path::Path, count: usize) -> Vec<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let lines = std::fs::read_to_string(path)
                .unwrap_or_default()
                // 子进程先追加 JSON 再写换行；只解析已经提交完整行的记录
                .split_inclusive('\n')
                .filter(|line| line.ends_with('\n'))
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<_>>();
            if lines.len() >= count {
                return lines;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observer marker")
}

#[tokio::test]
async fn frozen_plan_applies_scope_order_and_instance_deduplication() {
    let marker_directory = tempfile::tempdir().unwrap();
    let marker = marker_directory.path().join("observations.jsonl");
    let (cache, _store, runtime) = setup(
        vec![
            (
                "late",
                serde_json::json!({"observation_label":"late","observation_marker":marker}),
                vec![binding(EventKind::RequestCompleted, 20)],
            ),
            (
                "early",
                serde_json::json!({"observation_label":"early","observation_marker":marker}),
                vec![
                    binding(EventKind::RequestCompleted, 10),
                    binding(EventKind::WebSocketResponse, 10),
                ],
            ),
        ],
        RpcLimits::default(),
    )
    .await;
    let generation = gateway_core::runtime::extensions::ExtensionPreparationPort::prepare(
        &runtime,
        ConfigRevision::new(1).unwrap(),
    )
    .await
    .unwrap();
    let plan = runtime.observer_registry().resolve(&generation).unwrap();
    plan.dispatch(
        generation.clone(),
        observation("req_observed", RequestObservationOutcome::Succeeded)
            .with_provider(ProviderKind::new("openai").unwrap()),
    );
    let lines = wait_for_lines(&marker, 2).await;
    assert_eq!(
        lines.len(),
        2,
        "one completed event carries both terminal and usage facts"
    );
    assert_eq!(lines[0]["label"], "early");
    assert!(lines[0]["observation"]["terminal"].is_object());
    assert_eq!(lines[0]["observation"]["usage"]["total_tokens"], 18);
    assert_eq!(lines[1]["label"], "late");
    assert!(lines[1]["observation"]["terminal"].is_object());
    assert_eq!(lines[1]["observation"]["usage"]["total_tokens"], 18);

    plan.dispatch(
        generation.clone(),
        observation("req_model_miss", RequestObservationOutcome::Succeeded)
            .with_requested_model(PublicModelId::new("gpt-other").unwrap())
            .with_provider(ProviderKind::new("openai").unwrap()),
    );
    plan.dispatch(
        generation.clone(),
        observation("req_rejected", RequestObservationOutcome::Rejected),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(wait_for_lines(&marker, 2).await.len(), 2);
    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn websocket_observer_preserves_order_generation_and_complete_payload() {
    let marker_directory = tempfile::tempdir().unwrap();
    let hidden = marker_directory.path().join("hidden.jsonl");
    let readable = marker_directory.path().join("readable.jsonl");
    let (cache, _store, runtime) = setup_instances(
        vec![
            InstanceFixture {
                id: "hidden".into(),
                configuration: serde_json::json!({
                    "observation_label":"hidden",
                    "websocket_observation_marker":hidden,
                }),

                bindings: vec![binding(EventKind::WebSocketResponse, 1)],
            },
            InstanceFixture {
                id: "readable".into(),
                configuration: serde_json::json!({
                    "observation_label":"readable",
                    "websocket_observation_marker":readable,
                }),

                bindings: vec![binding(EventKind::WebSocketResponse, 2)],
            },
        ],
        RpcLimits::default(),
    )
    .await;
    let generation = gateway_core::runtime::extensions::ExtensionPreparationPort::prepare(
        &runtime,
        ConfigRevision::new(1).unwrap(),
    )
    .await
    .unwrap();
    let plan = runtime.observer_registry().resolve(&generation).unwrap();
    for (sequence, secret) in [(1, "first"), (2, "second")] {
        plan.dispatch_websocket_response(
            generation.clone(),
            websocket_observation(
                "req_websocket_ordered",
                sequence,
                serde_json::json!({"type":"response.output_text.delta","secret":secret}),
            ),
        );
    }
    plan.dispatch(
        generation.clone(),
        observation(
            "req_websocket_ordered",
            RequestObservationOutcome::Succeeded,
        ),
    );
    drop(plan);
    drop(generation);

    let hidden = wait_for_lines(&hidden, 2).await;
    let readable = wait_for_lines(&readable, 2).await;
    assert_eq!(
        hidden
            .iter()
            .map(|line| line["observation"]["sequence"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(hidden.iter().all(|line| {
        line["payload_bytes"].as_u64().unwrap() > 0
            && line["observation"]["payload_included"] == true
            && line["observation"]["event_type"] == "response.output_text.delta"
            && line["observation"]["account_id"] == "acct_observed"
    }));
    assert_eq!(
        readable
            .iter()
            .map(|line| line["observation"]["sequence"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(readable.iter().all(|line| {
        line["payload_bytes"].as_u64().unwrap() > 0
            && line["observation"]["payload_included"] == true
            && line["observation"]["event_type"] == "response.output_text.delta"
            && line["observation"]["account_id"] == "acct_observed"
    }));
    assert_eq!(
        readable
            .iter()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line["payload_text"].as_str().unwrap())
                    .unwrap()["secret"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn websocket_observer_bounds_event_count_and_payload_bytes_without_blocking() {
    let marker_directory = tempfile::tempdir().unwrap();
    let count_started = marker_directory.path().join("count-started.jsonl");
    let count_completed = marker_directory.path().join("count-completed.jsonl");
    let limits = RpcLimits {
        maximum_calls: 1,
        ..RpcLimits::default()
    };
    let (count_cache, _count_store, count_runtime) = setup_instances(
        vec![InstanceFixture {
            id: "count".into(),
            configuration: serde_json::json!({
                "websocket_observation_started_marker":count_started,
                "websocket_observation_marker":count_completed,
                "websocket_observation_delay_ms":150,
            }),

            bindings: vec![binding(EventKind::WebSocketResponse, 1)],
        }],
        limits,
    )
    .await;
    let count_generation = gateway_core::runtime::extensions::ExtensionPreparationPort::prepare(
        &count_runtime,
        ConfigRevision::new(1).unwrap(),
    )
    .await
    .unwrap();
    let count_plan = count_runtime
        .observer_registry()
        .resolve(&count_generation)
        .unwrap();
    count_plan.dispatch_websocket_response(
        count_generation.clone(),
        websocket_observation(
            "req_websocket_count",
            1,
            serde_json::json!({"type":"response.delta","value":1}),
        ),
    );
    wait_for_lines(&count_started, 1).await;
    for sequence in [2, 3] {
        count_plan.dispatch_websocket_response(
            count_generation.clone(),
            websocket_observation(
                "req_websocket_count",
                sequence,
                serde_json::json!({"type":"response.delta","value":sequence}),
            ),
        );
    }
    count_plan.dispatch(
        count_generation.clone(),
        observation("req_websocket_count", RequestObservationOutcome::Succeeded),
    );
    drop(count_plan);
    drop(count_generation);
    let completed = wait_for_lines(&count_completed, 2).await;
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0]["observation"]["sequence"], 1);
    assert_eq!(completed[1]["observation"]["sequence"], 2);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(wait_for_lines(&count_completed, 2).await.len(), 2);
    super::wait_until_empty(count_cache.path()).await;

    let byte_started = marker_directory.path().join("byte-started.jsonl");
    let fault_completed = marker_directory.path().join("fault-completed.jsonl");
    let healthy_completed = marker_directory.path().join("healthy-completed.jsonl");
    let (byte_cache, _byte_store, byte_runtime) = setup_instances(
        vec![
            InstanceFixture {
                id: "fault".into(),
                configuration: serde_json::json!({
                    "websocket_observation_started_marker":byte_started,
                    "websocket_observation_marker":fault_completed,
                    "websocket_observation_delay_ms":150,
                    "websocket_observation_fault":true,
                }),

                bindings: vec![binding(EventKind::WebSocketResponse, 1)],
            },
            InstanceFixture {
                id: "healthy".into(),
                configuration: serde_json::json!({
                    "websocket_observation_marker":healthy_completed,
                }),

                bindings: vec![binding(EventKind::WebSocketResponse, 2)],
            },
        ],
        RpcLimits::default(),
    )
    .await;
    let byte_generation = gateway_core::runtime::extensions::ExtensionPreparationPort::prepare(
        &byte_runtime,
        ConfigRevision::new(1).unwrap(),
    )
    .await
    .unwrap();
    let byte_plan = byte_runtime
        .observer_registry()
        .resolve(&byte_generation)
        .unwrap();
    let large_body = || {
        serde_json::json!({
            "type":"response.delta",
            "content":"x".repeat(600_000),
        })
    };
    byte_plan.dispatch_websocket_response(
        byte_generation.clone(),
        websocket_observation("req_websocket_bytes", 1, large_body()),
    );
    wait_for_lines(&byte_started, 1).await;
    byte_plan.dispatch_websocket_response(
        byte_generation.clone(),
        websocket_observation("req_websocket_bytes", 2, large_body()),
    );
    byte_plan.dispatch(
        byte_generation.clone(),
        observation("req_websocket_bytes", RequestObservationOutcome::Succeeded),
    );
    drop(byte_plan);
    drop(byte_generation);
    assert_eq!(wait_for_lines(&fault_completed, 1).await.len(), 1);
    let healthy = wait_for_lines(&healthy_completed, 1).await;
    assert_eq!(healthy.len(), 1, "观察失败不能阻断同批后续订阅者");
    assert_eq!(healthy[0]["observation"]["sequence"], 1);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(wait_for_lines(&healthy_completed, 1).await.len(), 1);
    super::wait_until_empty(byte_cache.path()).await;
}

#[tokio::test]
async fn observer_failure_continues_the_plan_and_timeout_is_bounded() {
    let marker_directory = tempfile::tempdir().unwrap();
    let started = marker_directory.path().join("started.jsonl");
    let completed = marker_directory.path().join("completed.jsonl");
    let (cache, _store, runtime) = setup(
        vec![
            (
                "fault",
                serde_json::json!({
                    "observation_label":"fault",
                    "observation_started_marker":started,
                    "observation_marker":completed,
                    "observation_fault":true,
                }),
                vec![binding(EventKind::RequestCompleted, 1)],
            ),
            (
                "timeout",
                serde_json::json!({
                    "observation_label":"timeout",
                    "observation_started_marker":started,
                    "observation_marker":completed,
                    "observation_delay_ms":2500,
                }),
                vec![binding(EventKind::RequestCompleted, 2)],
            ),
        ],
        RpcLimits::default(),
    )
    .await;
    let generation = gateway_core::runtime::extensions::ExtensionPreparationPort::prepare(
        &runtime,
        ConfigRevision::new(1).unwrap(),
    )
    .await
    .unwrap();
    let plan = runtime.observer_registry().resolve(&generation).unwrap();
    plan.dispatch(
        generation.clone(),
        observation("req_failure", RequestObservationOutcome::Succeeded)
            .with_provider(ProviderKind::new("openai").unwrap()),
    );
    let started_lines = wait_for_lines(&started, 2).await;
    assert_eq!(started_lines[0]["label"], "fault");
    assert_eq!(started_lines[1]["label"], "timeout");
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let completed_lines = wait_for_lines(&completed, 1).await;
    assert_eq!(completed_lines.len(), 1);
    assert_eq!(completed_lines[0]["label"], "fault");
    assert!(generation.is_ready(), "观察错误与超时不能击穿插件会话");
    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn invalid_observer_stage_policy_and_mixed_order_are_rejected_before_publish() {
    let mut invalid_stage = binding(EventKind::RequestCompleted, 1);
    invalid_stage.stage = "routing".into();
    let mut invalid_policy = binding(EventKind::RequestCompleted, 1);
    invalid_policy.failure_policy = PluginFailurePolicy::Reject;
    let mut invalid_websocket_stage = binding(EventKind::WebSocketResponse, 1);
    invalid_websocket_stage.stage = "stream".into();
    let mut invalid_websocket_policy = binding(EventKind::WebSocketResponse, 1);
    invalid_websocket_policy.failure_policy = PluginFailurePolicy::Delegate;
    let mut missing_event = binding(EventKind::RequestCompleted, 1);
    missing_event.event = None;
    let mut unknown_event = binding(EventKind::RequestCompleted, 1);
    unknown_event.event = Some("unknown".into());
    for bindings in [
        vec![missing_event],
        vec![unknown_event],
        vec![
            binding(EventKind::RequestCompleted, 1),
            binding(EventKind::RequestCompleted, 1),
        ],
        vec![invalid_stage],
        vec![invalid_policy],
        vec![invalid_websocket_stage],
        vec![invalid_websocket_policy],
        vec![
            binding(EventKind::RequestCompleted, 1),
            binding(EventKind::RequestCompleted, 2),
        ],
        vec![
            binding(EventKind::RequestCompleted, 1),
            binding(EventKind::WebSocketResponse, 2),
        ],
    ] {
        let (_cache, store, runtime) = setup(
            vec![("invalid", serde_json::json!({}), bindings)],
            RpcLimits::default(),
        )
        .await;
        let snapshot = store.snapshot.lock().unwrap().clone();
        assert!(
            gateway_admin::ports::plugins::PluginPreparation::prepare(&runtime, snapshot)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn client_scope_filters_observations_and_exposes_only_the_key_identifier() {
    let marker_directory = tempfile::tempdir().unwrap();
    let marker = marker_directory.path().join("scoped-observations.jsonl");
    let matching_group = "grp_11111111111111111111111111111111";
    let other_group = "grp_22222222222222222222222222222222";
    let mut scoped = binding(EventKind::RequestCompleted, 1);
    scoped.client_key_ids = vec!["client-key-observed".into()];
    scoped.account_group_ids = vec![matching_group.into()];
    scoped.provider_ids.clear();
    let (cache, _store, runtime) = setup(
        vec![(
            "scoped",
            serde_json::json!({"observation_label":"scoped","observation_marker":marker}),
            vec![scoped],
        )],
        RpcLimits::default(),
    )
    .await;
    let generation = gateway_core::runtime::extensions::ExtensionPreparationPort::prepare(
        &runtime,
        ConfigRevision::new(1).unwrap(),
    )
    .await
    .unwrap();
    let plan = runtime.observer_registry().resolve(&generation).unwrap();

    plan.dispatch(
        generation.clone(),
        observation_with_scope(
            "req_scope_match",
            RequestObservationOutcome::Rejected,
            "client-key-observed",
            vec![AccountGroupId::new(matching_group).unwrap()],
        ),
    );
    let lines = wait_for_lines(&marker, 1).await;
    assert_eq!(lines[0]["observation"]["request_id"], "req_scope_match");
    assert_eq!(lines[0]["observation"]["terminal"]["outcome"], "rejected");
    assert_eq!(
        lines[0]["observation"]["usage"]["failure"]["outcome"],
        "rejected"
    );
    assert_eq!(
        lines[0]["observation"]["usage"]["failure"]["send_state"],
        "not_sent"
    );
    assert_eq!(
        lines[0]["observation"]["usage"]["cost"]["status"],
        "unknown"
    );
    let wire = lines[0]["observation"].to_string();
    assert_eq!(
        lines[0]["observation"]["client_key_id"],
        "client-key-observed"
    );
    assert!(!wire.contains(matching_group));

    for (request_id, key, groups) in [
        (
            "req_key_miss",
            "client-key-other",
            vec![AccountGroupId::new(matching_group).unwrap()],
        ),
        (
            "req_group_miss",
            "client-key-observed",
            vec![AccountGroupId::new(other_group).unwrap()],
        ),
        ("req_all_accounts_miss", "client-key-observed", vec![]),
    ] {
        plan.dispatch(
            generation.clone(),
            observation_with_scope(request_id, RequestObservationOutcome::Rejected, key, groups),
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(wait_for_lines(&marker, 1).await.len(), 1);
    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn completed_observation_contains_terminal_cost_timings_and_failure() {
    let marker_directory = tempfile::tempdir().unwrap();
    let marker = marker_directory.path().join("usage-observations.jsonl");
    let (cache, _store, runtime) = setup(
        vec![(
            "usage",
            serde_json::json!({"observation_label":"usage","observation_marker":marker}),
            vec![binding(EventKind::RequestCompleted, 1)],
        )],
        RpcLimits::default(),
    )
    .await;
    let generation = gateway_core::runtime::extensions::ExtensionPreparationPort::prepare(
        &runtime,
        ConfigRevision::new(1).unwrap(),
    )
    .await
    .unwrap();
    let plan = runtime.observer_registry().resolve(&generation).unwrap();
    plan.dispatch(
        generation.clone(),
        observation("req_usage_failure", RequestObservationOutcome::Failed)
            .with_provider(ProviderKind::new("openai").unwrap())
            .with_attempt_count(2)
            .with_client_status_code(502)
            .with_upstream_status_code(503)
            .with_error_kind(GatewayErrorKind::UpstreamUnavailable)
            .with_retry_after_ms(1_500)
            .with_cost(
                ProviderReportedCost::from_usd_ticks(123_000_000)
                    .unwrap()
                    .into_estimate(),
            )
            .with_timings(ModelRequestTimings {
                first_text_ms: Some(12),
                latency_ms: Some(34),
                ..ModelRequestTimings::default()
            }),
    );
    let lines = wait_for_lines(&marker, 1).await;
    let observed = &lines[0]["observation"];
    assert_eq!(observed["terminal"]["outcome"], "failed");
    assert_eq!(observed["usage"]["cost"]["status"], "known");
    assert_eq!(observed["usage"]["cost"]["source"], "provider_reported");
    assert_eq!(observed["usage"]["cost"]["total"]["amount"], "0.0123");
    assert_eq!(observed["usage"]["cost"]["total"]["currency"], "USD");
    assert_eq!(observed["usage"]["timings"]["first_text_ms"], 12);
    assert_eq!(observed["usage"]["timings"]["latency_ms"], 34);
    assert_eq!(observed["usage"]["failure"]["outcome"], "failed");
    assert_eq!(observed["usage"]["failure"]["attempt_count"], 2);
    assert_eq!(observed["usage"]["failure"]["client_status_code"], 502);
    assert_eq!(observed["usage"]["failure"]["upstream_status_code"], 503);
    assert_eq!(
        observed["usage"]["failure"]["error_code"],
        "upstream_unavailable"
    );
    assert_eq!(observed["usage"]["failure"]["retry_after_ms"], 1_500);
    let wire = observed["usage"]["failure"].to_string();
    assert!(!wire.contains("message"));
    assert!(!wire.contains("raw_upstream_error"));
    assert!(!wire.contains("provider_error_code"));
    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn invalid_client_key_and_group_scopes_are_rejected_before_publish() {
    let mut duplicate_keys = binding(EventKind::RequestCompleted, 1);
    duplicate_keys.client_key_ids = vec!["client-key".into(), "client-key".into()];
    let mut invalid_group = binding(EventKind::RequestCompleted, 1);
    invalid_group.account_group_ids = vec!["not-a-group".into()];
    let mut duplicate_groups = binding(EventKind::RequestCompleted, 1);
    duplicate_groups.account_group_ids = vec![
        "grp_33333333333333333333333333333333".into(),
        "grp_33333333333333333333333333333333".into(),
    ];
    for bindings in [
        vec![duplicate_keys],
        vec![invalid_group],
        vec![duplicate_groups],
    ] {
        let (_cache, store, runtime) = setup(
            vec![("invalid-scope", serde_json::json!({}), bindings)],
            RpcLimits::default(),
        )
        .await;
        let snapshot = store.snapshot.lock().unwrap().clone();
        assert!(
            gateway_admin::ports::plugins::PluginPreparation::prepare(&runtime, snapshot)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn observer_backpressure_drops_excess_and_inflight_dispatch_keeps_generation_alive() {
    let marker_directory = tempfile::tempdir().unwrap();
    let started = marker_directory.path().join("started.jsonl");
    let completed = marker_directory.path().join("completed.jsonl");
    let limits = RpcLimits {
        maximum_calls: 1,
        ..RpcLimits::default()
    };
    let (cache, _store, runtime) = setup(
        vec![(
            "slow",
            serde_json::json!({
                "observation_label":"slow",
                "observation_started_marker":started,
                "observation_marker":completed,
                "observation_delay_ms":200,
            }),
            vec![binding(EventKind::RequestCompleted, 1)],
        )],
        limits,
    )
    .await;
    let generation = gateway_core::runtime::extensions::ExtensionPreparationPort::prepare(
        &runtime,
        ConfigRevision::new(1).unwrap(),
    )
    .await
    .unwrap();
    let plan = runtime.observer_registry().resolve(&generation).unwrap();
    plan.dispatch(
        generation.clone(),
        observation("req_first", RequestObservationOutcome::Succeeded)
            .with_provider(ProviderKind::new("openai").unwrap()),
    );
    wait_for_lines(&started, 1).await;
    plan.dispatch(
        generation.clone(),
        observation("req_excess", RequestObservationOutcome::Succeeded)
            .with_provider(ProviderKind::new("openai").unwrap()),
    );
    drop(plan);
    drop(generation);

    let completed = wait_for_lines(&completed, 1).await;
    assert_eq!(completed.len(), 1);
    assert_eq!(
        completed[0]["observation"]["request_id"], "req_first",
        "try-acquire backpressure must not queue a second observation"
    );
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn one_observer_matches_completed_and_websocket_scopes_independently() {
    let markers = tempfile::tempdir().unwrap();
    let completed = markers.path().join("completed.jsonl");
    let websocket = markers.path().join("websocket.jsonl");
    let mut completed_binding = binding(EventKind::RequestCompleted, 0);
    completed_binding.models = vec!["gpt-completed".into()];
    let mut websocket_binding = binding(EventKind::WebSocketResponse, 0);
    websocket_binding.models = vec!["gpt-stream".into()];
    let (cache, _store, runtime) = setup(
        vec![(
            "observer",
            serde_json::json!({
                "observation_marker": completed, "websocket_observation_marker": websocket,
            }),
            vec![completed_binding, websocket_binding],
        )],
        RpcLimits::default(),
    )
    .await;
    let generation = gateway_core::runtime::extensions::ExtensionPreparationPort::prepare(
        &runtime,
        ConfigRevision::new(1).unwrap(),
    )
    .await
    .unwrap();
    let plan = runtime.observer_registry().resolve(&generation).unwrap();
    for (id, model) in [
        ("req_matched", "gpt-completed"),
        ("req_missed", "gpt-stream"),
    ] {
        plan.dispatch(
            generation.clone(),
            observation(id, RequestObservationOutcome::Succeeded)
                .with_provider(ProviderKind::new("openai").unwrap())
                .with_requested_model(PublicModelId::new(model).unwrap()),
        );
    }
    for (id, model) in [
        ("req_matched_ws", "gpt-stream"),
        ("req_missed_ws", "gpt-completed"),
    ] {
        plan.dispatch_websocket_response(
            generation.clone(),
            websocket_observation(id, 1, serde_json::json!({"delta":"test"}))
                .with_requested_model(PublicModelId::new(model).unwrap()),
        );
    }
    assert_eq!(
        wait_for_lines(&completed, 1).await[0]["observation"]["request_id"],
        "req_matched"
    );
    assert_eq!(
        wait_for_lines(&websocket, 1).await[0]["observation"]["request_id"],
        "req_matched_ws"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(wait_for_lines(&completed, 1).await.len(), 1);
    assert_eq!(wait_for_lines(&websocket, 1).await.len(), 1);
    for id in ["req_matched_ws", "req_missed_ws"] {
        plan.dispatch(
            generation.clone(),
            observation(id, RequestObservationOutcome::Succeeded),
        );
    }
    drop(plan);
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}
