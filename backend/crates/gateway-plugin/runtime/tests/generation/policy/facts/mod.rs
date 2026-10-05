//! 验证插件可读取执行事实而不替换或重复原始执行

use super::*;
use futures::StreamExt;
use gateway_core::{
    engine::provider::{
        ProviderCallMetadata, ProviderSelectionObservation, ProviderStream,
        execute_attempt_middleware,
    },
    event::{
        FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseHeader,
        ProviderResponseMetadata, ProviderResponseObservation, ProviderResponseTimings,
        ResponseMeta,
    },
    metering::{
        CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates, Decimal, Money,
        ProviderReportedCost, Usage,
    },
    operation::{ExtensionSessionOwner, ProviderSessionState},
    routing::UpstreamModelId,
    upstream::{OpaqueUpstreamValue, UpstreamTransport},
};

#[tokio::test]
async fn facts_are_readable_without_replacing_or_duplicating_the_original_execution() {
    let records = tempfile::tempdir().unwrap();
    let marker = records.path().join("facts.jsonl");
    let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
    let package = crate::support::package_with_contributions(
        &worker,
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Attempt],
            vec!["openai".into()],
            vec!["openai".into()],
        )]),
    );
    let (_cache, runtime) = setup_package(vec![InstanceFixture {
        id: "facts-reader",
        configuration: serde_json::json!({"mode":"facts", "attempt":true, "facts_marker":marker}),
        bindings: vec![binding(MIDDLEWARE_CONTRIBUTION, "attempt", 0, PluginFailurePolicy::Reject)],
    }], package).await;
    let generation = prepare(&runtime).await;
    let plan = runtime.middleware_registry().resolve(&generation).unwrap();
    let metadata = ProviderCallMetadata::new(
        ProviderKind::new("openai").unwrap(),
        UpstreamModelId::new("native-model").unwrap(),
        ProviderAccountId::new("acct_original").unwrap(),
        UpstreamTransport::new("http").unwrap(),
    )
    .with_upstream_request_id(OpaqueUpstreamValue::new("upstream-original"))
    .with_selection_observation(ProviderSelectionObservation::new(17, None));
    let amount = Money::new(
        Decimal::from_scaled(123).unwrap(),
        gateway_core::metering::CurrencyCode::new("USD").unwrap(),
    );
    let breakdown = CalculatedCostBreakdown::new(
        CalculatedCostAmounts::new(amount, amount, amount, amount, amount, amount),
        CalculatedCostRates::new(amount, amount, amount, amount),
        Some("priority".into()),
        125,
    )
    .with_long_context_billing(true);
    let started = ProviderEvent::canonical(GatewayEvent::Started(ResponseMeta::new(
        "response-original",
        "native-model",
    )));
    let mut completed = ProviderEvent::canonical_with_wire(
        vec![
            GatewayEvent::Usage(Usage {
                input_tokens: Some(7),
                output_tokens: Some(2),
                ..Usage::default()
            }),
            GatewayEvent::CalculatedCost(breakdown.calculated_cost()),
            GatewayEvent::ProviderCost(ProviderReportedCost::from_usd_ticks(456).unwrap()),
            GatewayEvent::Completed(
                ResponseMeta::new("response-original", "native-model")
                    .with_finish_reason(FinishReason::Stop),
            ),
        ],
        ProtocolWireEvent::raw_json("openai", Bytes::from_static(br#"{ "output": [] }"#)).unwrap(),
    );
    completed.attach_observation(
        ProviderResponseObservation::new(UpstreamTransport::new("http").unwrap())
            .with_status_code(200)
            .with_request_id(OpaqueUpstreamValue::new("response-request-id"))
            .with_service_tier_if_valid("priority")
            .with_upstream_response_model_if_valid("returned-model")
            .with_client_headers(vec![ProviderResponseHeader::new(
                "set-cookie",
                Bytes::from_static(b"fixture=secret"),
            )])
            .with_provider_metadata(
                ProviderResponseMetadata::new("{\"private_field\":\"kept\"}".into()).unwrap(),
            )
            .with_timings(ProviderResponseTimings {
                headers_ms: Some(19),
                ..Default::default()
            }),
    );
    completed.attach_session_update(
        ProviderSessionState::new(
            "openai",
            serde_json::json!({"cursor":"private-cursor"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap()
        .with_extension_owner(ExtensionSessionOwner {
            instance_id: "owner".into(),
            contribution_id: "owner.adapter".into(),
            adapter_id: "adapter".into(),
            generation: 7,
            incarnation: "incarnation".into(),
            connection_local: true,
        }),
    );
    let mut stream = execute_attempt_middleware(
        Some(&plan),
        attempt_middleware_context(Arc::default()),
        operation(),
        ClientTransport::HttpJson,
        Box::new(move |_, _| {
            Box::pin(async move {
                Ok(ProviderStream::new(
                    metadata,
                    futures::stream::iter([Ok(started), Ok(completed)]),
                    (),
                ))
            })
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_original"
    );
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }
    assert_eq!(events.len(), 2);
    let costs = events
        .iter()
        .flat_map(ProviderEvent::canonical_facts)
        .filter(|fact| {
            matches!(
                fact,
                GatewayEvent::CalculatedCost(_) | GatewayEvent::ProviderCost(_)
            )
        })
        .count();
    assert_eq!(
        costs, 2,
        "reading and modifying snapshots must not change original cost facts"
    );
    let snapshots = marker_lines(&marker, 3).await;
    assert_eq!(snapshots[0]["provider_account_id"], "acct_original");
    assert_eq!(snapshots[0]["upstream_request_id"], "upstream-original");
    assert_eq!(
        snapshots[0]["selection_observation"]["account_selection_wait_ms"],
        17
    );
    assert_eq!(snapshots[1]["facts"][0]["type"], "started");
    let facts = &snapshots[2]["host"];
    assert_eq!(facts["costs"][0]["total"]["amount"], "0.0000000123");
    assert_eq!(
        facts["costs"][0]["breakdown"]["long_context_billing_applied"],
        true
    );
    assert_eq!(facts["costs"][1]["total"]["amount"], "0.0000000456");
    assert_eq!(
        facts["observation"]["client_headers"][0]["value"],
        serde_json::json!(b"fixture=secret".to_vec())
    );
    assert_eq!(
        facts["observation"]["provider_metadata"],
        "{\"private_field\":\"kept\"}"
    );
    assert_eq!(facts["observation"]["timings"]["headers_ms"], 19);
    assert_eq!(
        facts["session_update"]["payload"]["cursor"],
        "private-cursor"
    );
    assert_eq!(facts["session_update"]["extension_owner"]["generation"], 7);
    assert_eq!(facts["middleware_transformed"], false);
    assert!(facts["middleware_origin_wire"].is_null());
    drop(stream);
    runtime.shutdown().await;
}
