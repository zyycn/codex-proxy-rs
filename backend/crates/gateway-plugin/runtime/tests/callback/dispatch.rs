//! 验证回调方法精确分派，未知方法不进入其他能力的授权或资源处理

use gateway_admin::{
    model::plugins::management::PluginManagementRequest, ports::plugin_management::PluginManagement,
};
use serde_json::{Value, json};

#[tokio::test]
async fn unknown_callback_methods_are_unsupported_without_affecting_known_methods() {
    let Some(environment) = crate::support::environment::Environment::create().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let (runtime, core) = environment.plugin(json!({
        "management_registration":{"routes":[{"method":"GET","path":"dispatch","request_content_types":[],"response_content_types":["application/json"]}]},
        "data_queries":[
            {"method":"host.unknown","query":{}},
            {"method":"host.middleware.unknown","query":{}},
            {"method":"host.upstream.unknown","query":{}},
            {"method":"host.http.unknown","query":{}},
            {"method":gateway_plugin_sdk::call::host::HTTP_DO,"query":{}},
            {"method":gateway_plugin_sdk::call::middleware::NEXT_METHOD,"query":{}}
        ]
    })).await;
    let generation = core
        .snapshots()
        .acquire()
        .unwrap()
        .extensions()
        .unwrap()
        .clone();
    let view = runtime.views(&generation).await.unwrap().remove(0);
    let response = runtime
        .handle(
            &generation,
            &view.target,
            PluginManagementRequest {
                request_context: None,
                headers: Vec::new(),
                method: "GET".into(),
                path: "dispatch".into(),
                query: String::new(),
                content_type: None,
                body: Vec::new(),
                request_id: "callback-dispatch".into(),
            },
        )
        .await
        .unwrap();
    let results: Vec<Value> = serde_json::from_slice(&response.body).unwrap();
    let codes: Vec<_> = results
        .iter()
        .map(|result| result["error"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes,
        [
            "unsupported",
            "unsupported",
            "unsupported",
            "unsupported",
            "invalid_input",
            "permission_denied"
        ]
    );
    drop(generation);
    environment.release_plugin_accounts(&runtime);
    runtime.shutdown().await;
    drop(core);
    drop(runtime);
    environment.close().await;
}
