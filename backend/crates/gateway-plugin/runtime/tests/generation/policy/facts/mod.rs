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
async fn reading_websocket_facts_preserves_original_text_and_opaque_messages() {
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
    let plan = runtime
        .execution_registry()
        .middleware(&generation)
        .unwrap();
    let messages = [
        "{ \"type\" : \"future.event\", \"number\" : 1e3, \"text\" : \"\\u0061\" }".to_owned(),
        format!(
            "{{\"type\":\"future.deep\",\"value\":{}0{}}}",
            "[".repeat(140),
            "]".repeat(140)
        ),
        "future non-JSON text".to_owned(),
    ];
    let events: Vec<_> = messages
        .iter()
        .map(|raw| {
            let wire = match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(data) => ProtocolWireEvent::json("openai", None, data)
                    .unwrap()
                    .with_raw_websocket_message(raw.as_str()),
                Err(_) => ProtocolWireEvent::raw_websocket("openai", raw.as_str()).unwrap(),
            };
            Ok(ProviderEvent::wire(wire))
        })
        .collect();
    let mut stream = execute_attempt_middleware(
        Some(&plan),
        attempt_middleware_context_for_transport(Arc::default(), ClientTransport::WebSocket),
        operation(),
        ClientTransport::WebSocket,
        Box::new(move |_, _| {
            Box::pin(async move {
                Ok(ProviderStream::new(
                    ProviderCallMetadata::new(
                        ProviderKind::new("openai").unwrap(),
                        UpstreamModelId::new("native-model").unwrap(),
                        ProviderAccountId::new("acct_original").unwrap(),
                        UpstreamTransport::new("websocket").unwrap(),
                    ),
                    futures::stream::iter(events),
                    (),
                ))
            })
        }),
    )
    .await
    .unwrap();
    let mut delivered = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        assert!(!event.middleware_transformed());
        delivered.push(
            event
                .wire_event()
                .unwrap()
                .raw_websocket_message()
                .unwrap()
                .to_owned(),
        );
    }
    assert_eq!(delivered, messages);
    let snapshots = marker_lines(&marker, 4).await;
    for (snapshot, original) in snapshots[1..].iter().zip(&messages) {
        let event: gateway_plugin_sdk::call::model::ExecutionEvent =
            serde_json::from_value(snapshot.clone()).unwrap();
        let gateway_plugin_sdk::call::model::WirePayload::WebSocket { data, message, .. } =
            event.wire.unwrap().payload
        else {
            panic!("WebSocket facts must retain their original transport");
        };
        assert_eq!(message, original.as_bytes());
        assert_eq!(
            data.is_some(),
            serde_json::from_str::<serde_json::Value>(original).is_ok()
        );
    }
    drop(stream);
    runtime.shutdown().await;
}

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
    let plan = runtime
        .execution_registry()
        .middleware(&generation)
        .unwrap();
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
                upstream_response_ms: Some(7_000),
                upstream_engine_ms: Some(2.450638),
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
        facts["observation"]["timings"]["upstream_engine_ms"],
        2.450638
    );
    assert_eq!(
        facts["observation"]["timings"]["upstream_response_ms"],
        7_000
    );
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
