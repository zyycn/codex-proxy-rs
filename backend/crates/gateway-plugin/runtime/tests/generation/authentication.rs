use gateway_admin::model::client_keys::SetClientKeyEnabled;
use gateway_core::{
    engine::{authentication::ClientAuthenticationRequest, execution::ClientAuthenticationError},
    policy::ClientApiKeyId,
};
use serde_json::{Value, json};

use crate::support::environment::{Environment, account_grant, mutation};

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
    let (runtime, core) = environment
        .provider(configuration, vec![account_grant("accounts")])
        .await;
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
async fn rejection_unmapped_principal_and_plugin_supplied_key_id_fail_closed() {
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
    assert!(matches!(
        authentication,
        Err(ClientAuthenticationError::ProviderUnavailable)
    ));
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
