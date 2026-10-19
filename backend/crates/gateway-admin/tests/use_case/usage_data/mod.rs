//! 用量服务的 wire 精度、可信主体和独立 SDK 合同校验
use gateway_admin::model::usage_data::*;
use serde_json::json;
#[test]
fn counters_preserve_integer_precision_and_reject_json_numbers() {
    assert_eq!(
        serde_json::from_value::<UsageCount>(json!(u64::MAX.to_string()))
            .unwrap()
            .get(),
        u64::MAX
    );
    assert_eq!(
        serde_json::from_value::<UsageDelta>(json!(i64::MIN.to_string()))
            .unwrap()
            .get(),
        i64::MIN
    );
    for value in [json!(1), json!("1.5"), json!("+1")] {
        assert!(serde_json::from_value::<UsageCount>(value.clone()).is_err());
        assert!(serde_json::from_value::<UsageDelta>(value).is_err());
    }
}
#[tokio::test]
async fn usage_mutations_require_a_trusted_actor() {
    let admin = super::AdminHarness::new().build().await;
    let registry = admin.public_services();
    let denied = registry
        .call("usage.records.write", json!({"changes":[]}))
        .await
        .unwrap_err();
    assert!(denied.message.contains("trusted caller"));
    assert!(
        registry
            .call(
                "usage.records.write",
                json!({"changes":[],"actor":{"kind":"system"}})
            )
            .await
            .is_err()
    );
}
#[test]
fn sdk_wire_types_match_host_contracts() {
    use quote::ToTokens as _;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let items = |path: std::path::PathBuf| {
        syn::parse_file(&std::fs::read_to_string(path).unwrap())
            .unwrap()
            .items
            .into_iter()
            .filter(|item| {
                matches!(
                    item,
                    syn::Item::Struct(_)
                        | syn::Item::Enum(_)
                        | syn::Item::Impl(_)
                        | syn::Item::Type(_)
                )
            })
            .map(|item| item.into_token_stream().to_string())
            .collect::<Vec<_>>()
    };
    for name in [
        "value.rs",
        "record.rs",
        "query.rs",
        "aggregate.rs",
        "pricing.rs",
        "error.rs",
    ] {
        assert_eq!(
            items(root.join("src/model/usage_data").join(name)),
            items(
                root.join("../gateway-plugin/sdk/src/call/services/usage")
                    .join(name)
            ),
            "{name}"
        );
    }
}

#[tokio::test]
async fn batch_and_wire_limits_fail_before_reaching_storage() {
    let admin = super::AdminHarness::new().build().await;
    let registry = admin.public_services();
    let changes = (0..501)
        .map(|i| json!({"kind":"delete","id":format!("item_{i}")}))
        .collect::<Vec<_>>();
    let too_many = gateway_admin::public_service::scope_actor(
        UsageActor::System,
        registry.call("usage.records.write", json!({"changes":changes})),
    )
    .await
    .unwrap_err();
    assert_eq!(too_many.kind, "capacity_exceeded");
    let oversized = registry
        .call(
            "usage.records.get",
            json!({"id":"x".repeat(USAGE_DATA_MAX_REQUEST_BYTES)}),
        )
        .await
        .unwrap_err();
    assert_eq!(oversized.kind, "capacity_exceeded");
}
