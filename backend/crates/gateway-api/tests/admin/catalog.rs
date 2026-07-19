use chrono::Utc;
use gateway_api::admin::catalog::{
    CatalogListQuery, CreateProviderInstanceRequest, ProviderInstanceView,
};
use serde_json::json;

fn provider_request(provider_kind: &str) -> CreateProviderInstanceRequest {
    serde_json::from_value(json!({
        "id": format!("inst_{provider_kind}"),
        "expectedConfigRevision": 1,
        "providerKind": provider_kind,
        "name": provider_kind,
        "baseUrl": "https://example.invalid"
    }))
    .expect("decode provider instance")
}

#[test]
fn provider_instance_should_accept_openai_platform() {
    assert!(provider_request("openai").validate().is_ok());
}

#[test]
fn provider_instance_should_accept_xai_platform() {
    assert!(provider_request("xai").validate().is_ok());
}

#[test]
fn provider_instance_should_reject_removed_scheduling_fields() {
    let result = serde_json::from_value::<CreateProviderInstanceRequest>(json!({
        "id": "inst_openai",
        "expectedConfigRevision": 1,
        "providerKind": "openai",
        "name": "OpenAI",
        "baseUrl": "https://chatgpt.com",
        "maxConcurrency": 3
    }));

    assert!(result.is_err());
}

#[test]
fn provider_instance_should_reject_removed_model_routing_fields() {
    let result = serde_json::from_value::<CreateProviderInstanceRequest>(json!({
        "id": "inst_openai",
        "expectedConfigRevision": 1,
        "providerKind": "openai",
        "name": "OpenAI",
        "baseUrl": "https://chatgpt.com",
        "upstreamModelId": "gpt-5.4"
    }));

    assert!(result.is_err());
}

#[test]
fn provider_instance_should_reject_unknown_provider_kind() {
    let request = provider_request("unknown");

    assert_eq!(request.validate().unwrap_err().field(), "providerKind");
}

#[test]
fn provider_instance_should_reject_id_without_core_prefix() {
    let request: CreateProviderInstanceRequest = serde_json::from_value(json!({
        "id": "codex-official",
        "expectedConfigRevision": 1,
        "providerKind": "openai",
        "name": "Codex",
        "baseUrl": "https://chatgpt.com/backend-api"
    }))
    .expect("decode provider instance");

    assert_eq!(request.validate().unwrap_err().field(), "id");
}

#[test]
fn provider_instance_should_require_positive_revision() {
    let mut request = provider_request("openai");
    request.expected_config_revision = 0;

    assert_eq!(
        request.validate().unwrap_err().field(),
        "expectedConfigRevision"
    );
}

#[test]
fn catalog_page_size_should_remain_bounded() {
    let query = CatalogListQuery {
        cursor: None,
        limit: Some(201),
    };

    assert_eq!(query.validate().unwrap_err().field(), "limit");
}

#[test]
fn provider_instance_view_should_not_emit_provider_config_json() {
    let value = serde_json::to_value(ProviderInstanceView {
        id: "inst_openai".into(),
        provider_kind: "openai".into(),
        name: "Codex".into(),
        base_url: "https://chatgpt.com".into(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })
    .expect("serialize provider instance");

    assert!(value.get("configJson").is_none());
}

#[test]
fn provider_instance_view_should_emit_platform_explicitly() {
    let value = serde_json::to_value(ProviderInstanceView {
        id: "inst_xai".into(),
        provider_kind: "xai".into(),
        name: "xAI".into(),
        base_url: "https://grok.com".into(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })
    .expect("serialize provider instance");

    assert_eq!(value["providerKind"], "xai");
}
