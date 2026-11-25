//! 验证外部价格源的 Provider 选择、免费价格与无效文档拒绝

use gateway_admin::ports::pricing::{PricingCatalogSource, PricingSourceModel, PricingSourceScope};
use gateway_host::pricing;
use serde_json::json;

fn decode_catalog(
    body: &[u8],
) -> Result<gateway_admin::model::pricing::PricingSyncPreview, gateway_admin::model::AdminError> {
    pricing::decode_catalog(
        body,
        &scope(
            &["openai", "xai"],
            &["google", "google-vertex", "anthropic"],
        ),
    )
}

fn scope(direct: &[&str], references: &[&str]) -> PricingSourceScope {
    let mut sources: std::collections::BTreeMap<_, _> = direct
        .iter()
        .map(|vendor| (vendor.to_string(), PricingCatalogSource::Direct))
        .collect();
    sources.insert(
        "mapped-provider".into(),
        PricingCatalogSource::References(
            references
                .iter()
                .map(|vendor| {
                    (
                        vendor.to_string(),
                        PricingSourceModel {
                            provider: vendor.to_string(),
                            model: "example".into(),
                        },
                    )
                })
                .collect(),
        ),
    );
    PricingSourceScope { catalogs: sources }
}

#[test]
fn source_selects_supported_providers_and_preserves_free_prices() {
    let catalog = json!({
        "openai":{"models":{
            "free":{"modalities":{"output":["text"]},"cost":{"input":0,"output":0}},
            "normal":{"modalities":{"output":["text"]},"cost":{"input":2.5,"output":10,"cache_read":0.25}},
            "image":{"modalities":{"output":["image"]},"cost":{"input":1,"output":2}},
            "invalid":{"modalities":{"output":["text"]},"cost":{"input":-1,"output":2}},
            "wrong-tier":{"modalities":{"output":["text"]},"cost":{"input":1,"output":2,"tiers":[{"tier":{"size":200000},"input":2,"output":4}]}}
        }},
        "xai":{"models":{}},
        "unrelated":{"models":{"secret":{}}}
    });
    let result = decode_catalog(&serde_json::to_vec(&catalog).unwrap()).unwrap();
    let models = &result.prices["openai"];
    assert_eq!(models.len(), 2);
    assert_eq!(models["free"].bands["standard"].input.ticks_per_token(), 0);
    assert_eq!(
        models["normal"].bands["standard"]
            .cache_read
            .ticks_per_token(),
        2500
    );
    assert_eq!(result.skipped.len(), 3);
    assert!(!result.prices.contains_key("unrelated"));
}

#[test]
fn source_rejects_empty_partial_or_malformed_documents() {
    for document in [
        json!({}),
        json!({"openai":{"models":{}}}),
        json!({"openai":{"models":{}},"xai":{"models":{}}}),
    ] {
        assert!(decode_catalog(&serde_json::to_vec(&document).unwrap()).is_err());
    }
    assert!(decode_catalog(b"not json").is_err());
}

#[test]
fn source_exposes_google_anthropic_and_vertex_reference_prices_with_supported_context_tiers() {
    let source = json!({
        "openai":{"models":{}}, "xai":{"models":{}},
        "google":{"models":{
            "flash":{"modalities":{"output":["text"]},"cost":{"input":0.3,"output":2.5,"input_audio":1}},
            "pro":{"modalities":{"output":["text"]},"cost":{"input":1.25,"output":10,"tiers":[{"tier":{"type":"context","size":200000},"input":2.5,"output":15}]}},
            "image":{"modalities":{"output":["text","image"]},"cost":{"input":0.5,"output":3}}
        }},
        "anthropic":{"models":{"sonnet":{"modalities":{"output":["text"]},"cost":{"input":2,"output":10,"cache_write":2.5,"cache_read":0.1}}}},
        "google-vertex":{"models":{"openai/gpt-oss-120b-maas":{"modalities":{"output":["text"]},"cost":{"input":0.09,"output":0.36}}}}
    });
    let result = decode_catalog(&serde_json::to_vec(&source).unwrap()).unwrap();
    assert_eq!(
        result.prices["google"]["flash"].bands["standard"]
            .input
            .ticks_per_token(),
        3000
    );
    assert_eq!(
        result.prices["google"]["pro"].bands["long_standard"]
            .output
            .ticks_per_token(),
        150000
    );
    assert_eq!(
        result.prices["anthropic"]["sonnet"].bands["standard"]
            .cache_write
            .ticks_per_token(),
        25000
    );
    assert_eq!(
        result.prices["google-vertex"]["openai/gpt-oss-120b-maas"].bands["standard"]
            .output
            .ticks_per_token(),
        3600
    );
    assert_eq!(result.skipped, ["google/image"]);
}

#[test]
fn source_reads_the_registered_scope_without_a_host_allowlist() {
    let price = json!({"modalities":{"output":["text"]},"cost":{"input":1,"output":2}});
    let source = serde_json::to_vec(
        &json!({"custom-vendor":{"models":{"v1":price}}, "other":{"models":{"v1":price}}}),
    )
    .unwrap();
    let scope = scope(&["custom-vendor"], &["missing-reference"]);
    let preview = pricing::decode_catalog(&source, &scope).unwrap();
    assert_eq!(preview.prices.keys().collect::<Vec<_>>(), ["custom-vendor"]);
}
