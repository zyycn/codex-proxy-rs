//! 验证插件管理注册拒绝未声明资源、非法路由与缺失授权

use gateway_admin::ports::plugins::PluginPreparation;
use serde_json::json;

use super::{Environment, install, registration};

#[tokio::test]
async fn candidate_rejects_undeclared_resources_invalid_routes_and_missing_public_grants() {
    let Some(environment) = Environment::create().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    install(
        &environment,
        json!({"management_registration":registration()}),
    )
    .await;
    let (runtime, core) = environment.runtime().await;
    let source = environment
        .store
        .admin_ports()
        .plugins()
        .load_instances()
        .await
        .unwrap();
    for (field, value) in [
        (
            "routes",
            json!([{"method":"CONNECT","path":"tunnel","response_content_types":["application/json"]}]),
        ),
        (
            "routes",
            json!([{"method":"GET","path":"../admin","response_content_types":["application/json"]}]),
        ),
        (
            "routes",
            json!([{"method":"GET","path":"ok","response_content_types":["text/html\r\nSet-Cookie: attack"]}]),
        ),
        ("resources", json!([{"path":"bin/worker"}])),
        (
            "pages",
            json!([{"id":"bad","title":"Bad","entry":"ui/app.js"}]),
        ),
        (
            "pages",
            json!([{"id":"bad","title":"Bad","description":" ","entry":"ui/index.html"}]),
        ),
        (
            "pages",
            json!([{"id":"bad","title":"Bad","description":"line\ncontrol","entry":"ui/index.html"}]),
        ),
        (
            "pages",
            json!([{"id":"bad","title":"Bad","description":"x".repeat(513),"entry":"ui/index.html"}]),
        ),
    ] {
        let mut candidate = source.clone();
        candidate.instances[0].configuration["management_registration"][field] = value;
        assert!(runtime.prepare(candidate).await.is_err(), "{field}");
    }
    assert!(runtime.prepare(source).await.is_ok());
    drop(core);
    drop(runtime);
    environment.close().await;
}
