//! 验证运行设置编译、Key 默认值与显式请求覆盖的解析和继承

use std::{collections::BTreeMap, sync::Arc};

use gateway_core::{
    account::FastMode,
    account::OpaqueProviderData,
    identity::ProviderKind,
    policy::{ClientApiKeyId, ClientPolicy, PlaintextClientApiKey, RateLimits},
    routing::{ClientRoutingScope, ConfigRevision, FrozenAccountScope, RuntimeSnapshot},
    settings::{RequestSettings, SettingsValues},
};
use serde_json::json;

fn profiles(entries: &[(&str, &str)]) -> BTreeMap<ProviderKind, OpaqueProviderData> {
    entries
        .iter()
        .map(|(provider, name)| {
            (
                ProviderKind::new(*provider).unwrap(),
                OpaqueProviderData::new(json!({"identity":name}).as_object().unwrap().clone()),
            )
        })
        .collect()
}

fn snapshot(revision: u64, name: &str) -> Arc<RuntimeSnapshot> {
    Arc::new(
        RuntimeSnapshot::new(
            ConfigRevision::new(revision).unwrap(),
            SettingsValues::new(3, 50, "smart", BTreeMap::new(), None, None)
                .with_request_profiles(profiles(&[("openai", name), ("xai", name)])),
            vec![],
            vec![],
            vec![],
        )
        .unwrap(),
    )
}

fn policy(id: &str) -> ClientPolicy {
    ClientPolicy::new(
        ClientApiKeyId::new(id).unwrap(),
        PlaintextClientApiKey::new(format!("fixture-{id}")).unwrap(),
        Arc::new(
            FrozenAccountScope::new(Arc::default(), ClientRoutingScope::all_accounts())
                .with_request_profiles(profiles(&[("openai", id)]))
                .with_fast_mode(FastMode::Disabled),
        ),
        true,
        RateLimits {
            max_concurrency: 3,
            requests_per_minute: 10,
        },
    )
}

#[test]
fn settings_facts_are_shared_and_invalid_compilation_does_not_change_them() {
    let original = snapshot(1, "host");
    let frozen = original.as_ref().clone();
    assert!(std::ptr::eq(original.settings(), frozen.settings()));
    let facts = serde_json::to_value(original.settings()).unwrap();
    let changed = original
        .with_settings(
            &original
                .settings()
                .clone()
                .with_model_mappings(BTreeMap::from([("alias".into(), "model".into())])),
        )
        .unwrap();
    assert_eq!(changed.mapped_model("alias"), "model");
    assert_eq!(original.mapped_model("alias"), "alias");
    assert_eq!(serde_json::to_value(original.settings()).unwrap(), facts);
    assert!(
        original
            .with_settings(
                &original
                    .settings()
                    .clone()
                    .with_responses_max_decompressed_body_bytes(0)
            )
            .is_err()
    );
    assert!(std::ptr::eq(original.settings(), frozen.settings()));
}

#[test]
fn resolving_an_existing_policy_uses_key_defaults_and_the_current_host() {
    let key = policy("key");
    let first = RequestSettings::new(snapshot(1, "first")).apply_policy(key.clone());
    assert_eq!(
        first.account_scope().request_profiles(),
        &profiles(&[("openai", "key"), ("xai", "first")])
    );
    assert_eq!(
        first.defaults().request_profiles,
        key.defaults().request_profiles
    );
    let second = RequestSettings::new(snapshot(2, "second")).apply_policy(first);
    assert_eq!(
        second.account_scope().request_profiles(),
        &profiles(&[("openai", "key"), ("xai", "second")])
    );
    assert_eq!(
        second.defaults().request_profiles,
        profiles(&[("openai", "key")])
    );
}

#[test]
fn scope_changes_recompute_defaults_and_inherit_only_explicit_overrides() {
    let parent = policy("parent");
    let child = policy("child");
    let original = RequestSettings::new(snapshot(1, "first")).with_execution(&parent, Some(60_000));
    let mut values = original.execution_values().unwrap();
    values.runtime = values
        .runtime
        .with_model_mappings(BTreeMap::from([("alias".into(), "model".into())]));
    values.fast_mode = FastMode::Default;
    values.client_limits = RateLimits::unlimited();
    values.timeout_ms = Some(90_000);
    let changed = original.replace_execution(&values, "plugin").unwrap();
    assert!(
        changed.inspect()["overrides"]
            .get("request_profiles")
            .is_none()
    );
    let rebased = changed.rebase(snapshot(2, "second")).unwrap();
    let parent_values = rebased.execution_values().unwrap();
    assert_eq!(
        parent_values.runtime.request_profiles(),
        &profiles(&[("openai", "parent"), ("xai", "second")])
    );
    assert_eq!(parent_values.fast_mode, FastMode::Default);
    let child_settings = rebased.with_execution(&child, Some(60_000));
    let child_values = child_settings.execution_values().unwrap();
    assert_eq!(
        child_values.runtime.request_profiles(),
        &profiles(&[("openai", "child"), ("xai", "second")])
    );
    assert_eq!(child_values.fast_mode, FastMode::Disabled);
    assert_eq!(child_values.client_limits, child.limits());
    assert_eq!(child_values.timeout_ms, Some(60_000));
    assert_eq!(child_settings.snapshot().mapped_model("alias"), "model");
    assert_eq!(original.snapshot().mapped_model("alias"), "alias");
    assert_eq!(
        original.execution_values().unwrap().timeout_ms,
        Some(60_000)
    );
}

#[test]
fn explicitly_selecting_the_host_profiles_overrides_keys_after_rebase() {
    let host = snapshot(1, "first");
    let settings =
        RequestSettings::new(host.clone()).with_execution(&policy("parent"), Some(60_000));
    let mut values = settings.execution_values().unwrap();
    values.runtime = values
        .runtime
        .with_request_profiles(host.settings().request_profiles().clone());
    let changed = settings.replace_execution(&values, "plugin").unwrap();
    assert_eq!(
        changed.inspect()["overrides"]["request_profiles"]["instance_id"],
        "plugin"
    );
    let changed = changed
        .rebase(snapshot(2, "second"))
        .unwrap()
        .with_execution(&policy("child"), Some(60_000));
    let values = changed.execution_values().unwrap();
    let effective_policy = changed.apply_policy(policy("child"));
    assert_eq!(
        values.runtime.request_profiles(),
        host.settings().request_profiles()
    );
    assert_eq!(
        effective_policy.account_scope().request_profiles(),
        values.runtime.request_profiles()
    );
}

#[test]
fn changing_only_limits_reuses_the_resolved_account_scope() {
    let settings = RequestSettings::new(snapshot(1, "host"));
    let policy = settings.apply_policy(policy("key"));
    let scope = policy.account_scope().clone();
    let settings = settings.with_execution(&policy, Some(60_000));
    let mut values = settings.execution_values().unwrap();
    values.client_limits = RateLimits::unlimited();
    let settings = settings.replace_execution(&values, "plugin").unwrap();
    let resolved = settings.apply_policy(policy);
    assert!(Arc::ptr_eq(resolved.account_scope(), &scope));
    assert_eq!(resolved.limits(), RateLimits::unlimited());
    assert_eq!(resolved.defaults().limits.max_concurrency, 3);
}
