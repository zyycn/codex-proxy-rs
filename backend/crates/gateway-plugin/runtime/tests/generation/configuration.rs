//! 验证插件信任、配置 schema、敏感字段与贡献绑定约束

use gateway_admin::{
    model::plugins::instances::{PluginCapabilityBinding, PluginFailurePolicy},
    ports::plugins::PluginPreparation,
};
use gateway_plugin_sdk::{Capability, Contributions, Stage};

#[tokio::test]
async fn readiness_uses_validated_metadata_without_loading_the_archive() {
    let (_cache, store, runtime) = super::setup().await;
    let mut instance = store.snapshot.lock().unwrap().instances[0].clone();
    let mut metadata = store.artifacts[&instance.artifact_sha256].metadata.clone();
    // Store 中没有这个摘要；只读校验只能消费传入的声明，不能退回整包加载
    metadata.sha256 = "0".repeat(64);
    instance.artifact_sha256 = metadata.sha256.clone();
    metadata.configuration_schema = serde_json::json!({
        "type": "object", "required": ["name", "token"],
        "properties": {"name": {"type": "string"}, "token": {"type": "string"}},
        "additionalProperties": false
    });
    metadata.secret_fields = vec!["token".into()];
    instance.configuration = serde_json::json!({});
    assert!(
        !runtime
            .configuration_ready(instance.clone(), &metadata)
            .await
            .unwrap()
    );
    instance.configuration = serde_json::json!({"name": "fixture"});
    instance
        .secrets
        .insert("token".into(), "fixture-value".into());
    assert!(
        runtime
            .configuration_ready(instance.clone(), &metadata)
            .await
            .unwrap()
    );
    instance.configuration = serde_json::json!({"name": 42});
    assert!(
        runtime
            .configuration_ready(instance.clone(), &metadata)
            .await
            .is_err()
    );
    instance.configuration = serde_json::json!({"name": "fixture", "token": "not-allowed"});
    assert!(
        runtime
            .configuration_ready(instance.clone(), &metadata)
            .await
            .is_err()
    );
    instance.configuration = serde_json::json!({"name": "fixture"});
    instance
        .secrets
        .insert("undeclared".into(), "fixture-value".into());
    assert!(
        runtime
            .configuration_ready(instance, &metadata)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn schema_rejection_identifies_the_field_without_exposing_configuration_values() {
    let (_cache, store, runtime) = super::setup().await;
    let mut instance = store.snapshot.lock().unwrap().instances[0].clone();
    let mut metadata = store.artifacts[&instance.artifact_sha256].metadata.clone();
    metadata.configuration_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "timeout": {"type": "integer"},
            "token": {"type": "string", "pattern": "^accepted$"}
        },
        "additionalProperties": false
    });
    metadata.secret_fields = vec!["token".into()];
    for (configuration, secret, expected) in [
        (
            serde_json::json!({"timeout": "private-value"}),
            None,
            "插件配置 /timeout：类型不匹配",
        ),
        (
            serde_json::json!({"retired": "private-value"}),
            None,
            "插件配置 /retired：目标版本不支持此字段",
        ),
        (
            serde_json::json!({}),
            Some("private-value"),
            "插件配置 /token：不符合字段要求",
        ),
    ] {
        instance.configuration = configuration;
        instance.secrets.clear();
        if let Some(value) = secret {
            instance.secrets.insert("token".into(), value.into());
        }
        let error = runtime
            .configuration_ready(instance.clone(), &metadata)
            .await
            .unwrap_err();
        assert_eq!(error.message(), expected);
    }
}

#[tokio::test]
async fn process_execution_requires_explicit_trust() {
    let (_cache, store, runtime) = super::setup().await;
    let mut instance = store.snapshot.lock().unwrap().instances[0].clone();
    instance.trusted_process = false;
    assert!(runtime.validate(instance.clone()).await.is_err());
    instance.trusted_process = true;
    assert!(runtime.validate(instance).await.is_ok());
}

#[tokio::test]
async fn secrets_must_be_declared_by_the_package() {
    let (_cache, store, runtime) = super::setup().await;
    let mut instance = store.snapshot.lock().unwrap().instances[0].clone();
    instance
        .secrets
        .insert("token".into(), "sensitive-fixture".into());
    assert!(runtime.validate(instance).await.is_err());
}

#[tokio::test]
async fn enabled_bindings_must_reference_the_exact_declared_contribution_and_stage() {
    let (_cache, store, runtime) =
        super::setup_with_contributions(Contributions::from([crate::support::contribution(
            Capability::Observer,
            vec![Stage::Observation],
            vec![],
            vec![],
        )]))
        .await;
    let instance = store.snapshot.lock().unwrap().instances[0].clone();
    let mut valid = instance.clone();
    valid.bindings = vec![PluginCapabilityBinding {
        contribution: "test.example.observer".into(),
        stage: "observation".into(),
        order: 0,
        failure_policy: PluginFailurePolicy::Observe,
        client_key_ids: vec![],
        account_group_ids: vec![],
        provider_ids: vec![],
        models: vec![],
        event: Some("request_completed".into()),
        identity_bindings: vec![],
    }];
    assert!(runtime.validate(valid.clone()).await.is_ok());

    for (contribution, stage) in [
        ("test.example.unknown", "observation"),
        ("other.example.usage", "observation"),
        ("test.example.observer", "routing"),
    ] {
        let mut candidate = valid.clone();
        candidate.bindings[0].contribution = contribution.into();
        candidate.bindings[0].stage = stage.into();
        assert!(
            runtime.validate(candidate).await.is_err(),
            "binding {contribution}/{stage} must be rejected"
        );
    }
}
