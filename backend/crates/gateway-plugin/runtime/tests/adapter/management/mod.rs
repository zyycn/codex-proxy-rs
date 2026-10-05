//! 验证插件管理调用的身份持有、版本绑定与权限撤销

mod callback;
mod registration;

use std::collections::BTreeMap;

use gateway_admin::{
    PluginManagementService,
    model::{
        Revision,
        client_keys::SetClientKeyEnabled,
        plugins::{PluginSource, instances::PluginInstance, management::PluginManagementRequest},
    },
    ports::plugins::PluginPackageInspector,
};
use gateway_core::{policy::ClientApiKeyId, routing::ConfigRevision};
use gateway_plugin_runtime::{PackageInspector, PackageLimits};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::support::{
    self,
    environment::{Environment, mutation},
};

fn registration() -> Value {
    json!({
        "routes":[{"method":"POST","path":"echo","request_content_types":["application/octet-stream"],"response_content_types":["application/octet-stream"]}],
        "resources":[{"path":"ui/index.html"},{"path":"ui/app.js"},{"path":"ui/public.svg","public":true}],
        "pages":[{"id":"overview","title":"测试页面","description":"查看插件状态与调用结果","entry":"ui/index.html","icon":"ui/public.svg"}],
    })
}

async fn install(environment: &Environment, config: Value) -> PluginInstance {
    let mut files = BTreeMap::from([
        ("bin/worker".to_owned(), support::worker().to_vec()),
        (
            "ui/index.html".to_owned(),
            b"<!doctype html><title>Plugin</title><script src=app.js></script>".to_vec(),
        ),
        (
            "ui/app.js".to_owned(),
            b"parent.postMessage({kind:'loaded'}, '*')".to_vec(),
        ),
        (
            "ui/public.svg".to_owned(),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
        ),
    ]);
    let digests: BTreeMap<_, _> = files
        .iter()
        .map(|(name, bytes)| (name.clone(), hex::encode(Sha256::digest(bytes))))
        .collect();
    let manifest = json!({
        "manifestVersion":2,"name":"example","displayName":"管理示例","publisher":"management","version":"1.0.0",
        "engines":{"codex-proxy-rs":">=1.0.0, <2.0.0"},"main":"bin/worker","author":"test","description":"管理测试","license":"MIT",
        "runtime":"trustedProcess","resources":{"ui/index.html":"text/html","ui/app.js":"text/javascript","ui/public.svg":"image/svg+xml"},
        "contributes":{"management":{"id":"management.example.management","version":1,"stages":["management"],"inputFormats":[],"outputFormats":[]}},
        "package":{"protocolVersion":gateway_plugin_sdk::PROTOCOL_VERSION,"target":{"os":std::env::consts::OS,"architecture":std::env::consts::ARCH},"files":digests},
    });
    files.insert("plugin.json".into(), serde_json::to_vec(&manifest).unwrap());
    let artifact = PackageInspector::new(PackageLimits::default(), "1.0.0".parse().unwrap())
        .inspect(support::archive(files), None)
        .await
        .unwrap();
    let store = environment.store.admin_ports().plugins();
    let installed = store
        .install_artifact(artifact, PluginSource::Upload, &mutation())
        .await
        .unwrap();
    let installed = store
        .accept_artifact(&installed.artifact.metadata.sha256, &mutation())
        .await
        .unwrap();
    let instance = PluginInstance {
        id: uuid::Uuid::new_v4().to_string(),
        name: "管理页面".into(),
        artifact_sha256: installed.artifact.metadata.sha256,
        enabled: true,
        trusted_process: true,
        configuration: config,
        secrets: BTreeMap::new(),
        bindings: vec![],

        revision: Revision::new(1).unwrap(),
    };
    store
        .save_instance(instance, installed.config_revision, &mutation())
        .await
        .unwrap()
        .instance
}

fn request() -> PluginManagementRequest {
    PluginManagementRequest {
        headers: Vec::new(),
        method: "POST".into(),
        path: "echo".into(),
        query: "unicode=%E4%B8%AD".into(),
        content_type: Some("application/octet-stream".into()),
        body: vec![0, 255, 128, 10],
        request_id: "management-test".into(),
    }
}

#[tokio::test]
async fn management_model_stream_retains_selected_identity_until_completion_and_rechecks_revocation()
 {
    let Some(mut environment) = Environment::create_command().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let account = environment.account(None).await;
    let key_id = format!("key_{}", uuid::Uuid::new_v4().simple());
    environment
        .client_key(&key_id, "sk-management-bound-fixture")
        .await;
    let bound_nested = environment
        .directory
        .path()
        .join("management-bound-model.jsonl");
    let bound_calls = environment.directory.path().join("management-bound.jsonl");
    let unbound_nested = environment
        .directory
        .path()
        .join("management-unbound-model.jsonl");
    let nested_request = json!({
        "stream":true,
        "request":{
            "client_key_id":key_id,
            "model":crate::support::native::MODEL,
            "protocol":"openai",
            "operation":"generate",
            "provider":"openai",
            "account_id":account.as_str()
        },
        "body":{"model":crate::support::native::MODEL,"input":"bound management"}
    });
    let model_route = json!({
        "routes":[{
            "method":"POST",
            "path":"echo",
            "request_content_types":["application/octet-stream"],
            "response_content_types":["application/octet-stream"]
        }]
    });
    environment
        .install_plugin(
            json!({
                "plugin_id":"test.management-source",
                "management_registration":model_route,
                "management_nested_model_fixture":nested_request,
                "management_nested_model_marker":bound_nested,
                "nested_stream_trace_marker":environment.directory.path().join("management-stream.jsonl"),
                "management_marker":bound_calls,
            }),

        )
        .await;
    let mut denied_request = nested_request;
    denied_request["stream"] = json!(false);
    denied_request["request"]
        .as_object_mut()
        .unwrap()
        .remove("client_key_id");
    denied_request["expect_error"] = json!(true);
    environment
        .install_plugin(json!({
            "plugin_id":"test.management-unbound",
            "management_registration":model_route,
            "management_nested_model_fixture":denied_request,
            "management_nested_model_marker":unbound_nested,
        }))
        .await;
    let (runtime, core) = environment.runtime().await;
    let service = PluginManagementService::new(
        runtime.clone(),
        environment.store.admin_ports().plugins(),
        core.snapshots(),
    );
    let instances = environment
        .store
        .admin_ports()
        .plugins()
        .load_instances()
        .await
        .unwrap();
    let bound_id = &instances
        .instances
        .iter()
        .find(|instance| instance.configuration["plugin_id"] == "test.management-source")
        .unwrap()
        .id;
    let unbound_id = &instances
        .instances
        .iter()
        .find(|instance| instance.configuration["plugin_id"] == "test.management-unbound")
        .unwrap()
        .id;
    let views = service.views().await.unwrap();
    let bound = &views
        .iter()
        .find(|view| &view.target.instance_id == bound_id)
        .unwrap()
        .target;
    let unbound = &views
        .iter()
        .find(|view| &view.target.instance_id == unbound_id)
        .unwrap()
        .target;
    environment.store.start_command_line_writes().unwrap();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            service.handle(bound, request()),
        )
        .await
        .unwrap_or_else(|_| {
            let trace = std::fs::read_to_string(
                environment.directory.path().join("management-stream.jsonl"),
            );
            panic!("管理模型流未完成：{trace:?}");
        })
        .unwrap()
        .status,
        200,
    );
    assert_eq!(
        service.handle(unbound, request()).await.unwrap().status,
        200
    );
    assert_eq!(
        std::fs::read_to_string(&bound_nested)
            .unwrap()
            .lines()
            .count(),
        1
    );
    let unbound_marker: Value =
        serde_json::from_str(std::fs::read_to_string(&unbound_nested).unwrap().trim()).unwrap();
    assert_eq!(unbound_marker["error"], "invalid_input");
    assert_eq!(
        environment.wait_for_bound_model_requests(&key_id, 1).await,
        vec![("plugin_bound_model".into(), bound_id.clone())]
    );

    let (revision, _) = environment
        .store
        .admin_ports()
        .client_keys()
        .set_client_key_enabled(
            SetClientKeyEnabled {
                id: ClientApiKeyId::new(key_id).unwrap(),
                enabled: false,
            },
            &mutation(),
        )
        .await
        .unwrap();
    core.snapshot_control()
        .publish_committed(ConfigRevision::new(revision.get()).unwrap())
        .await;
    assert!(service.handle(bound, request()).await.is_err());
    assert_eq!(
        std::fs::read_to_string(bound_calls)
            .unwrap()
            .lines()
            .count(),
        2,
        "Key revocation is checked by the model callback, not before entering the plugin"
    );

    drop(service);
    drop(core);
    drop(runtime);
    environment
        .store
        .shutdown_command_line_writes()
        .await
        .unwrap();
    environment.close().await;
}

#[tokio::test]
async fn invalid_management_responses_stop_the_plugin_but_business_statuses_do_not() {
    for (response, faulted) in [
        (
            json!({"status":"invalid","content_type":"application/octet-stream"}),
            true,
        ),
        (
            json!({"status":429,"content_type":"application/octet-stream"}),
            false,
        ),
    ] {
        let Some(environment) = Environment::create().await else {
            return;
        };
        install(
            &environment,
            json!({"management_registration":registration(),"management_response":response}),
        )
        .await;
        let (runtime, core) = environment.runtime().await;
        let service = PluginManagementService::new(
            runtime.clone(),
            environment.store.admin_ports().plugins(),
            core.snapshots(),
        );
        let views = service.views().await.unwrap();
        let result = service.handle(&views[0].target, request()).await;
        if faulted {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().status, 429);
        }
        assert_eq!(
            core.snapshots()
                .acquire()
                .unwrap()
                .extensions()
                .unwrap()
                .is_ready(),
            !faulted
        );
        for probe in core.health_probes() {
            assert!(matches!(
                probe.check().await,
                gateway_core::health::HealthState::Healthy
            ));
        }
        assert!(
            environment
                .store
                .admin_ports()
                .plugins()
                .load_instances()
                .await
                .unwrap()
                .instances[0]
                .enabled
        );
        drop(service);
        drop(core);
        drop(runtime);
        environment.close().await;
    }
}

#[tokio::test]
async fn management_resources_and_raw_calls_are_version_bound_and_revocation_removes_pages() {
    let Some(environment) = Environment::create().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let marker = environment.directory.path().join("management-calls.jsonl");
    install(
        &environment,
        json!({"management_registration":registration(),"management_marker":marker,"management_echo_headers":true}),
    )
    .await;
    let (runtime, core) = environment.runtime().await;
    let service = PluginManagementService::new(
        runtime.clone(),
        environment.store.admin_ports().plugins(),
        core.snapshots(),
    );
    let views = service.views().await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].pages[0].id, "overview");
    assert_eq!(
        views[0].pages[0].description.as_deref(),
        Some("查看插件状态与调用结果")
    );
    assert_eq!(views[0].resources.len(), 3);
    let target = &views[0].target;
    assert_eq!(
        service
            .resource(target, "ui/index.html", false)
            .await
            .unwrap()
            .content_type,
        "text/html"
    );
    assert!(
        service
            .resource(target, "ui/index.html", true)
            .await
            .is_err()
    );
    assert_eq!(
        service
            .resource(target, "ui/public.svg", true)
            .await
            .unwrap()
            .body
            .as_ref(),
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"
    );
    for path in [
        "../bin/worker",
        "ui/../app.js",
        "%2e%2e/bin/worker",
        "ui//index.html",
        "bin/worker",
    ] {
        assert!(service.resource(target, path, false).await.is_err());
    }
    assert!(!marker.exists(), "静态资源读取不能执行插件 handler");
    let mut raw_request = request();
    raw_request.headers = vec![
        gateway_core::engine::middleware::MiddlewareHeader::new(
            "authorization",
            "Bearer fixture".into(),
        ),
        gateway_core::engine::middleware::MiddlewareHeader::new("set-cookie", "first=1".into()),
        gateway_core::engine::middleware::MiddlewareHeader::new(
            "set-cookie",
            bytes::Bytes::from_static(&[0xff]),
        ),
    ];
    let response = service.handle(target, raw_request).await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body.as_ref(), &[0, 255, 128, 10]);
    assert_eq!(
        response
            .headers
            .iter()
            .map(|header| (header.name(), header.value().as_ref()))
            .collect::<Vec<_>>(),
        [
            ("authorization", b"Bearer fixture".as_slice()),
            ("set-cookie", b"first=1".as_slice()),
            ("set-cookie", [0xff].as_slice()),
        ]
    );
    let mut invalid = request();
    invalid.content_type = Some("text/html".into());
    assert!(service.handle(target, invalid).await.is_err());
    let mut invalid = request();
    invalid.path = "unregistered".into();
    assert!(service.handle(target, invalid).await.is_err());
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
    let store = environment.store.admin_ports().plugins();
    let mut snapshot = store.load_instances().await.unwrap();
    let mut instance = snapshot.instances.remove(0);
    instance.enabled = false;
    store
        .save_instance(instance, snapshot.config_revision, &mutation())
        .await
        .unwrap();
    // 模拟持久撤销已提交但旧发布视图仍存在；页面与资源都不能继续使用旧权限
    assert!(service.views().await.unwrap().is_empty());
    assert!(service.handle(target, request()).await.is_err());
    assert!(
        service
            .resource(target, "ui/public.svg", true)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
    drop(service);
    drop(core);
    drop(runtime);
    environment.close().await;
}
