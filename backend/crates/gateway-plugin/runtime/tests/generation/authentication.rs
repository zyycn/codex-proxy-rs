//! 验证插件认证的身份映射、回退条件与发布后的权限撤销

use gateway_admin::model::client_keys::SetClientKeyEnabled;
use gateway_core::{
    engine::{authentication::ClientAuthenticationRequest, execution::ClientAuthenticationError},
    policy::ClientApiKeyId,
};
use serde_json::{Value, json};

use crate::support::environment::{Environment, mutation};

async fn authenticate(
    configuration: Value,
    authorization: &str,
) -> Option<(
    Environment,
    std::sync::Arc<gateway_plugin_runtime::PluginRuntime>,
    gateway_core::CoreBundle,
    Result<gateway_core::engine::execution::AuthenticatedClient, ClientAuthenticationError>,
)> {
    let environment = Environment::create().await?;
    environment
        .client_key("key_frontend", "sk-native-fixture")
        .await;
    let (runtime, core) = environment.plugin(configuration).await;
    let result = core
        .execution_service()
        .authenticate_request(ClientAuthenticationRequest::new(authorization).unwrap())
        .await;
    Some((environment, runtime, core, result))
}

async fn close(
    environment: Environment,
    runtime: std::sync::Arc<gateway_plugin_runtime::PluginRuntime>,
    core: gateway_core::CoreBundle,
) {
    environment.release_plugin_accounts(&runtime);
    drop(core);
    drop(runtime);
    environment.close().await;
}

fn configuration(result: Value, exclusive: bool, expected_authorization: &str) -> Value {
    json!({
        "frontend_authentication_result": result,
        "frontend_authentication_principal": "external-user",
        "frontend_authentication_client_key_id": "key_frontend",
        "frontend_authentication_exclusive": exclusive,
        "expected_frontend_authorization": expected_authorization,
        "extra_result_fields": true,
    })
}

#[tokio::test]
async fn frontend_authentication_maps_only_the_configured_principal_to_an_existing_policy() {
    let Some((environment, runtime, core, result)) = authenticate(
        configuration(
            json!({"outcome":"authenticated","principal":"external-user"}),
            true,
            "External controlled-fixture",
        ),
        "External controlled-fixture",
    )
    .await
    else {
        return;
    };

    let client = result.expect("configured principal authenticates");
    assert_eq!(client.policy().key_id().as_str(), "key_frontend");
    close(environment, runtime, core).await;
}

#[tokio::test]
async fn delegate_falls_back_only_for_not_matched_while_exclusive_never_falls_back() {
    let Some((delegating_environment, delegating_runtime, delegating_core, delegated)) =
        authenticate(
            configuration(
                json!({"outcome":"not_matched"}),
                false,
                "Bearer sk-native-fixture",
            ),
            "Bearer sk-native-fixture",
        )
        .await
    else {
        return;
    };
    assert_eq!(
        delegated.unwrap().policy().key_id().as_str(),
        "key_frontend"
    );
    close(delegating_environment, delegating_runtime, delegating_core).await;

    let Some((exclusive_environment, exclusive_runtime, exclusive_core, exclusive)) = authenticate(
        configuration(
            json!({"outcome":"not_matched"}),
            true,
            "Bearer sk-native-fixture",
        ),
        "Bearer sk-native-fixture",
    )
    .await
    else {
        return;
    };
    assert!(matches!(
        exclusive,
        Err(ClientAuthenticationError::InvalidKey)
    ));
    close(exclusive_environment, exclusive_runtime, exclusive_core).await;
}

#[tokio::test]
async fn authentication_rejects_unmapped_principals_and_ignores_plugin_selected_keys() {
    for result in [
        json!({"outcome":"rejected"}),
        json!({"outcome":"authenticated","principal":"unmapped"}),
    ] {
        let Some((environment, runtime, core, authentication)) = authenticate(
            configuration(result, false, "Bearer sk-native-fixture"),
            "Bearer sk-native-fixture",
        )
        .await
        else {
            return;
        };
        assert!(matches!(
            authentication,
            Err(ClientAuthenticationError::InvalidKey)
        ));
        assert!(
            core.snapshots()
                .acquire()
                .unwrap()
                .extensions()
                .unwrap()
                .is_ready()
        );
        close(environment, runtime, core).await;
    }

    let Some((environment, runtime, core, authentication)) = authenticate(
        configuration(
            json!({
                "outcome":"authenticated",
                "principal":"external-user",
                "client_key_id":"attacker-selected-key"
            }),
            false,
            "Bearer sk-native-fixture",
        ),
        "Bearer sk-native-fixture",
    )
    .await
    else {
        return;
    };
    assert_eq!(
        authentication.unwrap().policy().key_id().as_str(),
        "key_frontend"
    );
    assert!(
        core.snapshots()
            .acquire()
            .unwrap()
            .extensions()
            .unwrap()
            .is_ready()
    );
    for probe in core.health_probes() {
        assert!(matches!(
            probe.check().await,
            gateway_core::health::HealthState::Healthy
        ));
    }
    close(environment, runtime, core).await;
}

#[tokio::test]
async fn disabling_the_mapped_client_key_revokes_frontend_authentication_after_publish() {
    let Some((environment, runtime, core, first)) = authenticate(
        configuration(
            json!({"outcome":"authenticated","principal":"external-user"}),
            true,
            "External controlled-fixture",
        ),
        "External controlled-fixture",
    )
    .await
    else {
        return;
    };
    assert!(first.is_ok());

    let (revision, _) = environment
        .store
        .admin_ports()
        .client_keys()
        .set_client_key_enabled(
            SetClientKeyEnabled {
                id: ClientApiKeyId::new("key_frontend").unwrap(),
                enabled: false,
            },
            &mutation(),
        )
        .await
        .unwrap();
    core.snapshot_control()
        .publish_committed(gateway_core::routing::ConfigRevision::new(revision.get()).unwrap())
        .await;
    let revoked = core
        .execution_service()
        .authenticate_request(
            ClientAuthenticationRequest::new("External controlled-fixture").unwrap(),
        )
        .await;
    assert!(matches!(
        revoked,
        Err(ClientAuthenticationError::InvalidKey)
    ));
    close(environment, runtime, core).await;
}

#[tokio::test]
async fn startup_failure_of_an_authentication_plugin_never_falls_back_to_a_native_key() {
    let mut config = configuration(
        json!({"outcome":"not_matched"}),
        false,
        "Bearer sk-native-fixture",
    );
    config["startup"] = json!("fail");
    let Some((environment, runtime, core, authentication)) =
        authenticate(config, "Bearer sk-native-fixture").await
    else {
        return;
    };
    assert!(
        core.snapshots().acquire().is_ok(),
        "the host stays available"
    );
    assert!(matches!(
        authentication,
        Err(ClientAuthenticationError::ProviderUnavailable)
    ));
    for probe in core.health_probes() {
        assert!(matches!(
            probe.check().await,
            gateway_core::health::HealthState::Healthy
        ));
    }
    close(environment, runtime, core).await;
}

#[tokio::test]
async fn startup_failure_of_an_unrelated_plugin_preserves_native_authentication_and_health() {
    let Some((environment, runtime, core, authentication)) =
        authenticate(json!({"startup":"fail"}), "Bearer sk-native-fixture").await
    else {
        return;
    };
    assert_eq!(
        authentication.unwrap().policy().key_id().as_str(),
        "key_frontend"
    );
    assert!(core.snapshots().acquire().is_ok());
    for probe in core.health_probes() {
        assert!(matches!(
            probe.check().await,
            gateway_core::health::HealthState::Healthy
        ));
    }
    close(environment, runtime, core).await;
}

#[tokio::test]
async fn callback_verification_reenters_the_authentication_instance_without_model_binding() {
    assert_callback_identity_queries(true).await;
}

#[tokio::test]
async fn client_queries_do_not_require_model_execution_ports() {
    assert_callback_identity_queries(false).await;
}

async fn assert_callback_identity_queries(include_models: bool) {
    use gateway_admin::{
        model::plugins::management::PluginManagementRequest,
        ports::plugin_management::PluginManagement,
    };
    use gateway_plugin_runtime::RpcLimits;
    let Some(environment) = Environment::create().await else {
        return;
    };
    environment
        .client_key("key_frontend", "sk-native-fixture")
        .await;
    let account = environment.account(None).await;
    let marker = environment
        .directory
        .path()
        .join("authenticated-facts.json");
    let mut facts = vec![json!(
        {"method":"host.data.access.check", "query":{"client_key_id":"key_frontend", "account_ids":[account.as_str(), "acct_missing"]}}
    )];
    if include_models {
        facts.push(json!(
            {"method":"host.models.list", "params":{"client_key_id":"key_frontend", "protocol":"codex", "client_version":"0.154.0"}}
        ));
    }
    let mut config = configuration(
        json!({"outcome":"authenticated","principal":"external-user"}),
        true,
        "External controlled-fixture",
    );
    config["sdk_session"] = json!(true);
    config["frontend_authentication_marker"] = json!(marker);
    config["auth_queries"] = json!(facts);
    config["management_registration"] = json!({"routes":[{"method":"GET","path":"facts","request_content_types":[],"response_content_types":["application/json"]}]});
    let mut queries = vec![json!(
        {"method":"host.client.verify", "query":{"authorization":"External controlled-fixture"}}
    )];
    queries.extend(facts);
    config["data_queries"] = json!(queries);
    environment.install_plugin(config).await;
    let (runtime, core) = environment
        .runtime_with_identity_port(
            RpcLimits {
                maximum_executing_calls: 1,
                maximum_queue_timeout: std::time::Duration::from_millis(500),
                ..RpcLimits::default()
            },
            crate::support::native::provider_registry(),
        )
        .await;
    if include_models {
        runtime
            .bind_model_ports(
                &core.nested_model_execution_port(),
                &core.affinity_lookup_port(),
            )
            .unwrap();
    }
    let snapshot = core.snapshots().acquire().unwrap();
    let generation = snapshot.extensions().unwrap();
    let view = runtime.views(generation).await.unwrap().remove(0);
    let response = runtime
        .handle(
            generation,
            &view.target,
            PluginManagementRequest {
                request_context: None,
                headers: Vec::new(),
                method: "GET".into(),
                path: "facts".into(),
                query: String::new(),
                content_type: None,
                body: Vec::new(),
                request_id: "verify-composition".into(),
            },
        )
        .await
        .unwrap();
    let results: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(results[0], json!({"client_key_id":"key_frontend"}));
    let authenticated_facts: Value =
        serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
    assert_eq!(authenticated_facts[0], results[1]);
    assert_eq!(
        authenticated_facts[0]["accounts"],
        json!([
            {"account_id":account.as_str(),"allowed":true}, {"account_id":"acct_missing","allowed":false}
        ])
    );
    if include_models {
        assert_eq!(authenticated_facts[1], results[2]);
        // 此替身只登记执行能力，没有原生目录；查询应成功返回空目录而不是执行绑定错误
        assert_eq!(results[2], json!({"models":[]}));
    }
    assert!(
        environment
            .bound_model_requests("key_frontend")
            .await
            .is_empty()
    );
    drop(snapshot);
    close(environment, runtime, core).await;
}
