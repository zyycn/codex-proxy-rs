use async_trait::async_trait;
use gateway_admin::model::plugins::distribution::{
    DownloadedPlugin, GithubReleaseQuery, PluginRelease, RemotePluginLocation, SourceCredential,
    SourceCredentialInfo,
};
use gateway_admin::model::plugins::distribution::{PluginSourceBinding, PluginUpdateSource};
use gateway_admin::{
    model::{
        AdminError, MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactIcon,
            PluginArtifactIconResource, PluginArtifactMetadata, PluginArtifactMutation,
            PluginIconTheme, PluginSource,
            state::{
                ApplyPluginStateMigration, DeletePluginState, PluginStateConfiguration,
                PluginStateMigrationBatch, PluginStateOwner, PluginStateOwnerRequest,
                PluginStateRecord, PluginStateTransition, PluginStateWrite, PutPluginState,
            },
        },
    },
    ports::{
        plugins::{
            PluginPackageInspector, PluginStateStore, PluginStateStoreError,
            PluginStateStoreErrorKind, PluginStateStoreResult, PluginStore,
        },
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use std::{collections::BTreeMap, sync::Arc};

mod management;

#[test]
fn artifact_metadata_wire_exposes_v2_identity_and_contributions() {
    use gateway_admin::model::plugins::{
        PluginArtifactIcon, PluginArtifactIconVariants, PluginArtifactMetadata, PluginContribution,
    };

    let metadata = PluginArtifactMetadata {
        plugin_id: "test.example".into(),
        version: "1.0.0".into(),
        name: "example".into(),
        display_name: "Example".into(),
        publisher: "test".into(),
        author: None,
        description: "fixture".into(),
        license: "MIT".into(),
        sha256: "a".repeat(64),
        platforms: vec!["linux-x86_64".into()],
        icon: Some(PluginArtifactIcon::Themed(PluginArtifactIconVariants {
            light: "assets/icon-light.png".into(),
            dark: "assets/icon-dark.webp".into(),
        })),
        contributes: BTreeMap::from([(
            "middleware".into(),
            PluginContribution {
                id: "test.example.middleware".into(),
                version: 1,
                stages: vec!["attempt".into()],
                input_formats: vec!["openai".into()],
                output_formats: vec!["openai".into()],
            },
        )]),
        requested_permissions: vec![],
        configuration_schema: serde_json::json!({}),
        secret_fields: vec![],
        state_namespaces: vec![],
    };
    let value = serde_json::to_value(metadata).expect("v2 artifact metadata JSON");
    assert_eq!(value["displayName"], "Example");
    assert_eq!(value["publisher"], "test");
    assert!(value["author"].is_null());
    assert_eq!(value["icon"]["light"], "assets/icon-light.png");
    assert_eq!(value["icon"]["dark"], "assets/icon-dark.webp");
    assert_eq!(
        value["contributes"]["middleware"]["id"],
        "test.example.middleware"
    );
    assert!(value.get("capabilities").is_none());
    assert!(value.get("permissionDescriptions").is_none());
}

#[test]
fn instance_binding_wire_uses_contribution_without_capability_alias() {
    use gateway_admin::model::plugins::instances::PluginCapabilityBinding;

    let binding = serde_json::from_value::<PluginCapabilityBinding>(serde_json::json!({
        "contribution": "test.example.usage",
        "stage": "observation",
        "order": 0,
        "failurePolicy": "observe"
    }))
    .expect("v2 contribution binding");
    assert_eq!(binding.contribution, "test.example.usage");

    assert!(
        serde_json::from_value::<PluginCapabilityBinding>(serde_json::json!({
            "capability": "usage",
            "stage": "observation",
            "order": 0,
            "failurePolicy": "observe"
        }))
        .is_err(),
        "legacy capability binding must be rejected"
    );
}

#[tokio::test]
async fn artifact_icon_is_admin_only_digest_bound_and_hardened() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;

    let fixture = super::AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let digest = "f".repeat(64);
    let response = gateway_api::admin::router::<super::AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/admin/plugins/artifacts/{digest}/icon?theme=light"
                ))
                .header("cookie", "cpr_session=valid-session")
                .header("x-request-id", "req_plugin_icon")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        headers["content-security-policy"],
        "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'"
    );
    assert_eq!(
        headers["cache-control"],
        "private, max-age=31536000, immutable"
    );
    assert_eq!(body.as_ref(), b"icon fixture");

    let response = gateway_api::admin::router::<super::AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/admin/plugins/artifacts/{digest}/icon?theme=dark"
                ))
                .header("cookie", "cpr_session=valid-session")
                .header("x-request-id", "req_plugin_svg_icon")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/svg+xml");
    assert_eq!(
        response.headers()["content-security-policy"],
        headers["content-security-policy"]
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        response.headers()["cache-control"],
        headers["cache-control"]
    );
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(
        body.as_ref(),
        br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"/>"#
    );

    let response = gateway_api::admin::router::<super::AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/admin/plugins/artifacts/{digest}/icon?theme=light"
                ))
                .header("x-request-id", "req_plugin_icon_unauthorized")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = gateway_api::admin::router::<super::AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/admin/plugins/artifacts/{}/icon?theme=light",
                    "e".repeat(64)
                ))
                .header("cookie", "cpr_session=valid-session")
                .header("x-request-id", "req_plugin_icon_missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn distribution_rate_limit_keeps_its_safe_owner_message() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;

    let fixture = super::AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let direct = fixture
        .services
        .plugins()
        .query_release(
            GithubReleaseQuery {
                repository: "rate-limit/fixture".to_owned(),
                tag: None,
                allow_prerelease: false,
            },
            &[],
            None,
        )
        .await
        .expect_err("fixture release query should be rate limited");
    assert_eq!(
        direct.kind(),
        gateway_admin::model::AdminErrorKind::RateLimited
    );
    let response = gateway_api::admin::router::<super::AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/plugins/releases/query")
                .header("content-type", "application/json")
                .header("cookie", "cpr_session=valid-session")
                .header("x-request-id", "req_plugin_distribution_rate_limit")
                .body(Body::from(
                    serde_json::json!({
                        "query": {
                            "repository": "rate-limit/fixture",
                            "tag": null,
                            "allowPrerelease": false
                        },
                        "credentialIds": []
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert!(response.headers().get("retry-after").is_none());
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("rate limited response body");
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).expect("rate limited response JSON"),
        serde_json::json!({
            "code": 42902,
            "message": "插件来源限流，请稍后重试",
            "data": null
        })
    );
}

#[tokio::test]
async fn instance_list_serializes_safe_runtime_diagnostics() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;

    let fixture = super::AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let direct = fixture
        .services
        .plugins()
        .instances()
        .await
        .expect("direct plugin instance projection");
    assert_eq!(direct.len(), 1);
    let response = gateway_api::admin::router::<super::AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/plugins/instances")
                .header("cookie", "cpr_session=valid-session")
                .header("x-request-id", "req_plugin_instance_diagnostics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("response body");
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let value: serde_json::Value = serde_json::from_slice(&body).expect("response json");
    let instance = &value["data"][0];
    assert_eq!(instance["running"], false);
    assert_eq!(instance["runtime"]["status"], "preparation_failed");
    assert_eq!(instance["runtime"]["actualRevision"], 6);
    assert_eq!(
        instance["runtime"]["drainingRevisions"],
        serde_json::json!([5])
    );
    assert_eq!(instance["runtime"]["retainedContinuations"], 1);
    assert_eq!(instance["runtime"]["failure"]["code"], "unavailable");
    assert_eq!(
        instance["runtime"]["failure"]["message"],
        "插件候选准备失败"
    );
    assert!(instance["runtime"].get("configuration").is_none());
}

#[tokio::test]
async fn plugin_upload_endpoints_require_admin_and_bound_raw_bodies() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;

    let fixture = super::AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for path in ["artifacts/upload", "artifacts/upload/verify"] {
        for authenticated in [false, true] {
            let mut request = Request::builder()
                .method("POST")
                .uri(format!("/api/admin/plugins/{path}"))
                .header("x-request-id", "req_plugin_upload_limit")
                .header("content-type", "application/octet-stream");
            if authenticated {
                request = request.header("cookie", "cpr_session=valid-session");
            }
            let response = gateway_api::admin::router::<super::AdminTestState>()
                .with_state(fixture.state())
                .oneshot(
                    request
                        .body(Body::from(vec![0_u8; 32 * 1024 * 1024 + 1]))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if authenticated {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::UNAUTHORIZED
                },
                "{path}"
            );
        }
    }
}

#[tokio::test]
async fn artifact_acceptance_returns_the_flat_install_contract() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;

    let fixture = super::AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = gateway_api::admin::router::<super::AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/plugins/artifacts/accept")
                .header("content-type", "application/json")
                .header("cookie", "cpr_session=valid-session")
                .header("x-request-id", "req_plugin_accept")
                .body(Body::from(format!(r#"{{"sha256":"{}"}}"#, "a".repeat(64))))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let result = &value["data"];
    assert_eq!(result["configRevision"], 7);
    assert_eq!(result["defaultInstanceId"], serde_json::Value::Null);
    assert_eq!(result["configurationRequired"], false);
    assert_eq!(result["artifact"]["metadata"]["sha256"], "a".repeat(64));
    assert_eq!(
        result["artifact"]["metadata"]["permissionDescriptions"][0]["permission"],
        "models"
    );
    assert_eq!(
        result["artifact"]["metadata"]["permissionDescriptions"][0]["label"],
        "当前宿主标签"
    );
    assert!(result["artifact"]["acceptedAt"].is_string());
    assert!(result.get("mutation").is_none());
}

#[tokio::test]
async fn removed_instance_policy_fields_and_extra_accept_fields_are_rejected() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;

    let fixture = super::AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (path, body) in [
        (
            "/api/admin/plugins/instances",
            serde_json::json!({
                "name":"Example",
                "artifactSha256":"a".repeat(64),
                "enabled":false,
                "trustedProcess":true,
                "configuration":{},
                "grants":[],
                "bindings":[]
            }),
        ),
        (
            "/api/admin/plugins/artifacts/accept",
            serde_json::json!({"sha256":"a".repeat(64),"trusted":true}),
        ),
    ] {
        let response = gateway_api::admin::router::<super::AdminTestState>()
            .with_state(fixture.state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .header("cookie", "cpr_session=valid-session")
                    .header("x-request-id", "req_plugin_strict_input")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path}"
        );
    }
}

#[tokio::test]
async fn plugin_mutations_require_admin_and_validate_json_on_static_post_routes() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;

    let fixture = super::AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for path in [
        "artifacts/accept",
        "artifacts/delete",
        "artifacts/install",
        "releases/query",
        "updates/check",
        "artifacts/verify",
        "source-credentials",
        "source-credentials/delete",
        "update-sources",
        "instances",
        "instances/update",
        "instances/rollback",
        "instances/switch-version",
        "instances/disable",
        "instances/delete",
    ] {
        for authenticated in [false, true] {
            let mut request = Request::builder()
                .method("POST")
                .uri(format!("/api/admin/plugins/{path}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_plugin_routes");
            if authenticated {
                request = request.header("cookie", "cpr_session=valid-session");
            }
            let response = gateway_api::admin::router::<super::AdminTestState>()
                .with_state(fixture.state())
                .oneshot(request.body(Body::from("{}")).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if authenticated {
                    // 字段缺失沿用 AdminJson 的结构校验合同。
                    StatusCode::UNPROCESSABLE_ENTITY
                } else {
                    StatusCode::UNAUTHORIZED
                },
                "{path}"
            );
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
    }
}

#[derive(Default)]
pub(super) struct TestPluginPorts {
    instances:
        std::sync::Mutex<Option<Vec<gateway_admin::model::plugins::instances::PluginInstance>>>,
}

#[async_trait]
impl gateway_admin::ports::plugin_management::PluginManagement for TestPluginPorts {
    async fn authorize_models(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::management::PluginManagementTarget,
    ) -> Result<(), AdminError> {
        Ok(())
    }

    async fn start_callback(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::management::PluginManagementTarget,
        _: gateway_admin::model::plugins::management::StartPluginManagementCallback,
        _: &gateway_admin::model::auth::AdminRequestContext,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementCallbackTicket, AdminError>
    {
        Err(AdminError::not_found("unused plugin fixture"))
    }
    async fn callback(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::management::PluginManagementTarget,
        _: &str,
        _: gateway_admin::model::plugins::management::PluginManagementRequest,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementResponse, AdminError>
    {
        Ok(management::resource_fixture("ui/index.html"))
    }
    async fn views(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
    ) -> Result<Vec<gateway_admin::model::plugins::management::PluginManagementView>, AdminError>
    {
        Ok(Vec::new())
    }
    async fn resource(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::management::PluginManagementTarget,
        path: &str,
        public: bool,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementResponse, AdminError>
    {
        if public && path != "public/login.css" {
            return Err(AdminError::not_found("resource is not public"));
        }
        Ok(management::resource_fixture(path))
    }
    async fn handle(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::management::PluginManagementTarget,
        _: gateway_admin::model::plugins::management::PluginManagementRequest,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementResponse, AdminError>
    {
        Ok(management::resource_fixture("api/echo"))
    }
}

#[async_trait]
impl gateway_admin::ports::plugins::PluginPreparation for TestPluginPorts {
    async fn configuration_ready(
        &self,
        _: gateway_admin::model::plugins::instances::PluginInstance,
        _: &gateway_admin::model::plugins::PluginArtifactMetadata,
    ) -> Result<bool, AdminError> {
        Ok(true)
    }

    async fn validate(
        &self,
        _: gateway_admin::model::plugins::instances::PluginInstance,
    ) -> Result<gateway_admin::model::plugins::state::PluginStateConfiguration, AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }
    async fn prepare(
        &self,
        _: gateway_admin::model::Revision,
        _: gateway_admin::model::plugins::instances::PluginInstanceSnapshot,
    ) -> Result<gateway_core::runtime::extensions::ExtensionSetReference, AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }

    async fn runtime_diagnostics(
        &self,
        snapshot: &gateway_admin::model::plugins::instances::PluginInstanceSnapshot,
        _: Option<u64>,
        _: Option<&gateway_core::runtime::extensions::ExtensionSetReference>,
    ) -> Option<BTreeMap<String, gateway_admin::model::plugins::instances::PluginInstanceRuntime>>
    {
        Some(
            snapshot
                .instances
                .iter()
                .map(|instance| {
                    (
                        instance.id.clone(),
                        gateway_admin::model::plugins::instances::PluginInstanceRuntime {
                            status: gateway_admin::model::plugins::instances::PluginInstanceRuntimeStatus::PreparationFailed,
                            actual_revision: Some(6),
                            actual_artifact_sha256: Some("b".repeat(64)),
                            failure: Some(gateway_admin::model::plugins::instances::PluginInstanceRuntimeFailure {
                                code: "unavailable".into(),
                                message: "插件候选准备失败".into(),
                            }),
                            draining_revisions: vec![5],
                            retained_continuations: 1,
                        },
                    )
                })
                .collect(),
        )
    }
}

fn unavailable() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "plugin",
        "unused plugin fixture",
    )
}

fn icon_artifact(digest: &str) -> InspectedPluginArtifact {
    InspectedPluginArtifact {
        metadata: PluginArtifactMetadata {
            plugin_id: "test.icon".into(),
            version: "1.0.0".into(),
            name: "icon".into(),
            display_name: "Icon fixture".into(),
            publisher: "test".into(),
            author: None,
            description: "fixture".into(),
            license: "MIT".into(),
            sha256: digest.into(),
            platforms: vec!["linux-x86_64".into()],
            icon: (digest.as_bytes()[0] == b'f')
                .then(|| PluginArtifactIcon::Path("assets/icon.png".into())),
            contributes: BTreeMap::new(),
            requested_permissions: Vec::new(),
            configuration_schema: serde_json::json!({}),
            secret_fields: Vec::new(),
            state_namespaces: Vec::new(),
        },
        archive: b"icon archive fixture".as_slice().into(),
    }
}

fn accepted_artifact() -> InstalledPluginArtifact {
    let mut metadata = icon_artifact(&"a".repeat(64)).metadata;
    metadata.requested_permissions = vec!["models".into()];
    InstalledPluginArtifact {
        metadata,
        source: PluginSource::Upload,
        installed_at: chrono::Utc::now(),
        accepted_at: Some(chrono::Utc::now()),
    }
}

fn state_unavailable<T>() -> PluginStateStoreResult<T> {
    Err(PluginStateStoreError::new(
        PluginStateStoreErrorKind::Unavailable,
    ))
}

#[async_trait]
impl PluginStateStore for TestPluginPorts {
    async fn load_owner(
        &self,
        _: PluginStateOwnerRequest,
    ) -> PluginStateStoreResult<Option<PluginStateOwner>> {
        state_unavailable()
    }
    async fn get(
        &self,
        _: &PluginStateOwner,
        _: &str,
        _: &str,
    ) -> PluginStateStoreResult<Option<PluginStateRecord>> {
        state_unavailable()
    }
    async fn put(
        &self,
        _: &PluginStateOwner,
        _: PutPluginState,
    ) -> PluginStateStoreResult<PluginStateWrite> {
        state_unavailable()
    }
    async fn delete(
        &self,
        _: &PluginStateOwner,
        _: DeletePluginState,
    ) -> PluginStateStoreResult<bool> {
        state_unavailable()
    }
    async fn transition_required(
        &self,
        _: &str,
        _: &PluginStateConfiguration,
    ) -> PluginStateStoreResult<bool> {
        state_unavailable()
    }
    async fn begin_transition(
        &self,
        _: &str,
        _: Revision,
        _: &str,
        _: PluginStateConfiguration,
    ) -> PluginStateStoreResult<PluginStateTransition> {
        state_unavailable()
    }
    async fn migration_batch(
        &self,
        _: &str,
        _: &str,
        _: u32,
    ) -> PluginStateStoreResult<PluginStateMigrationBatch> {
        state_unavailable()
    }
    async fn apply_migration_batch(
        &self,
        _: ApplyPluginStateMigration,
    ) -> PluginStateStoreResult<()> {
        state_unavailable()
    }
    async fn abort_transition(&self, _: &str) -> PluginStateStoreResult<()> {
        state_unavailable()
    }
}

#[async_trait]
impl PluginStore for TestPluginPorts {
    async fn management_target_is_current(
        &self,
        target: &gateway_admin::model::plugins::management::PluginManagementTarget,
    ) -> AdminStoreResult<bool> {
        Ok(self
            .load_instances()
            .await?
            .instances
            .iter()
            .any(|instance| {
                instance.enabled
                    && instance.trusted_process
                    && instance.id == target.instance_id
                    && instance.artifact_sha256 == target.artifact_sha256
                    && instance.revision.get() == target.revision
            }))
    }
    async fn load_instances(
        &self,
    ) -> AdminStoreResult<gateway_admin::model::plugins::instances::PluginInstanceSnapshot> {
        Ok(
            gateway_admin::model::plugins::instances::PluginInstanceSnapshot {
                config_revision: Revision::new(7).unwrap(),
                instances: self.instances.lock().unwrap().clone().unwrap_or_else(|| {
                    vec![gateway_admin::model::plugins::instances::PluginInstance {
                        id: "plugin-instance-diagnostic".into(),
                        name: "Diagnostic fixture".into(),
                        artifact_sha256: "a".repeat(64),
                        enabled: true,
                        trusted_process: true,
                        configuration: serde_json::json!({}),
                        secrets: BTreeMap::new(),
                        grants: Vec::new(),
                        bindings: Vec::new(),
                        revision: Revision::new(7).unwrap(),
                    }]
                }),
            },
        )
    }
    async fn save_instance(
        &self,
        _: gateway_admin::model::plugins::instances::PluginInstance,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::plugins::instances::PluginInstanceMutation> {
        Err(unavailable())
    }
    async fn delete_instance(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }
    async fn list_update_sources(&self) -> AdminStoreResult<Vec<PluginSourceBinding>> {
        Ok(Vec::new())
    }
    async fn change_update_source(
        &self,
        _: PluginSourceBinding,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }

    async fn list_source_credentials(&self) -> AdminStoreResult<Vec<SourceCredentialInfo>> {
        Ok(Vec::new())
    }
    async fn load_source_credential(&self, _: &str) -> AdminStoreResult<SourceCredential> {
        Err(unavailable())
    }
    async fn save_source_credential(
        &self,
        _: SourceCredential,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }
    async fn delete_source_credential(
        &self,
        _: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }

    async fn list_artifacts(&self) -> AdminStoreResult<Vec<InstalledPluginArtifact>> {
        Ok(vec![accepted_artifact()])
    }
    async fn load_artifact(&self, digest: &str) -> AdminStoreResult<InspectedPluginArtifact> {
        if matches!(digest.as_bytes().first(), Some(b'e' | b'f')) && digest.len() == 64 {
            return Ok(icon_artifact(digest));
        }
        Err(unavailable())
    }
    async fn install_artifact(
        &self,
        _: InspectedPluginArtifact,
        _: PluginSource,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        Err(unavailable())
    }
    async fn accept_artifact(
        &self,
        digest: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        if digest != "a".repeat(64) {
            return Err(unavailable());
        }
        Ok(PluginArtifactMutation {
            config_revision: Revision::new(7).unwrap(),
            artifact: accepted_artifact(),
        })
    }
    async fn delete_artifact(&self, _: &str, _: &MutationContext) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }
}

#[async_trait]
impl PluginPackageInspector for TestPluginPorts {
    fn permission_descriptions(
        &self,
        permissions: &[String],
    ) -> Vec<gateway_admin::model::plugins::PluginPermissionDescription> {
        permissions
            .iter()
            .map(
                |permission| gateway_admin::model::plugins::PluginPermissionDescription {
                    permission: permission.clone(),
                    label: "当前宿主标签".into(),
                    description: "当前宿主说明".into(),
                },
            )
            .collect()
    }

    async fn inspect(
        &self,
        _: Arc<[u8]>,
        _: Option<String>,
    ) -> Result<InspectedPluginArtifact, AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }

    async fn icon(
        &self,
        _: Arc<[u8]>,
        expected_sha256: String,
        theme: PluginIconTheme,
    ) -> Result<Option<PluginArtifactIconResource>, AdminError> {
        Ok(
            (expected_sha256 == "f".repeat(64)).then(|| PluginArtifactIconResource {
                content_type: match theme {
                    PluginIconTheme::Light => "image/png",
                    PluginIconTheme::Dark => "image/svg+xml",
                }
                .into(),
                body: match theme {
                    PluginIconTheme::Light => b"icon fixture".to_vec(),
                    PluginIconTheme::Dark => {
                        br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"/>"#.to_vec()
                    }
                },
            }),
        )
    }
}

#[async_trait]
impl gateway_admin::ports::plugins::PluginDistribution for TestPluginPorts {
    fn validate_source(&self, _: &PluginUpdateSource) -> Result<(), AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }

    fn validate_credential(&self, _: &SourceCredential) -> Result<(), AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }
    async fn query_release(
        &self,
        query: GithubReleaseQuery,
        _: Vec<SourceCredential>,
        _: Option<gateway_admin::model::plugins::distribution::PluginDistributionEgress>,
    ) -> Result<PluginRelease, AdminError> {
        if query.repository == "rate-limit/fixture" {
            return Err(AdminError::new(
                gateway_admin::model::AdminErrorKind::RateLimited,
                "插件来源限流，请稍后重试",
            ));
        }
        Err(AdminError::invalid("unused plugin fixture"))
    }
    async fn download(
        &self,
        _: RemotePluginLocation,
        _: Vec<SourceCredential>,
        _: Option<gateway_admin::model::plugins::distribution::PluginDistributionEgress>,
    ) -> Result<DownloadedPlugin, AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }
}

#[tokio::test]
async fn version_plan_requires_admin_and_an_explicit_target() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt as _;
    let fixture = super::AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for authenticated in [false, true] {
        let mut request = Request::builder()
            .uri("/api/admin/plugins/instances/version-plan")
            .header("x-request-id", "version-plan");
        if authenticated {
            request = request.header("cookie", "cpr_session=valid-session");
        }
        let response = gateway_api::admin::router::<super::AdminTestState>()
            .with_state(fixture.state())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if authenticated {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::UNAUTHORIZED
            }
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
}
