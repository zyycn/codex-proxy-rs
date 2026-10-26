//! 使用公开 SDK 驱动真实子进程，覆盖受管任务的并发与恢复

use gateway_plugin_sdk::{
    ErrorCode, Frame, PluginFault,
    client::{
        CallFuture, CallReply, PluginCall, PluginHandler, PluginSession, SessionConfig, write_frame,
    },
};
use serde_json::json;
use tokio::io::AsyncReadExt as _;

pub(super) async fn run(hello: Frame, input: tokio::io::Stdin) {
    let mut prefix = Vec::new();
    write_frame(&mut prefix, &hello).await.unwrap();
    let input = std::io::Cursor::new(prefix).chain(input);
    let session = PluginSession::accept(input, tokio::io::stdout(), SessionConfig::default())
        .await
        .unwrap();
    let handler = Handler {
        configuration: session.handshake().configuration.clone(),
        contributes: session.handshake().contributes.clone(),
    };
    session.run(handler).await.unwrap();
}

struct Handler {
    configuration: serde_json::Value,
    contributes: gateway_plugin_sdk::Contributions,
}

impl Handler {
    async fn queries(&self, call: &PluginCall, key: &str) -> Vec<serde_json::Value> {
        let mut results = Vec::new();
        for query in self.configuration[key].as_array().unwrap() {
            let metadata = query.get("params").is_some() || query["method"] == "host.keys.list";
            let params = if metadata {
                query.get("params").unwrap_or(&query["query"]).clone()
            } else {
                json!({})
            };
            let payload = if metadata {
                query["body"]
                    .as_str()
                    .map_or_else(Vec::new, |body| body.as_bytes().to_vec())
            } else {
                serde_json::to_vec(&query["query"]).unwrap()
            };
            let result = call
                .host
                .call(query["method"].as_str().unwrap(), params, payload)
                .await;
            results.push(match result {
                Ok(reply) if metadata => {
                    assert!(reply.payload.is_empty());
                    reply.result
                }
                Ok(reply) => {
                    assert_eq!(reply.result, json!({}));
                    serde_json::from_slice(&reply.payload).unwrap()
                }
                Err(error) => json!({"error":error.into_plugin_fault().code}),
            });
        }
        results
    }
}

impl PluginHandler for Handler {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            match call.method.as_str() {
                "plugin.register" => Ok(CallReply::unary(
                    serde_json::to_value(gateway_plugin_sdk::call::registration::Registration {
                        contributes: self.contributes.clone(),
                    })
                    .unwrap(),
                    Vec::new(),
                )),
                "management.register" => Ok(CallReply::unary(
                    json!({}),
                    serde_json::to_vec(&self.configuration["management_registration"]).unwrap(),
                )),
                "frontend_auth.identifier" => Ok(CallReply::unary(
                    json!({"identifier":"fixture-auth"}),
                    Vec::new(),
                )),
                "frontend_auth.authenticate" => {
                    let request: gateway_plugin_sdk::call::frontend_authentication::FrontendAuthenticationRequest = serde_json::from_slice(&call.payload).unwrap();
                    assert_eq!(
                        request.authorization,
                        self.configuration["expected_frontend_authorization"]
                            .as_str()
                            .unwrap()
                    );
                    let facts = if self.configuration.get("auth_queries").is_some() {
                        self.queries(&call, "auth_queries").await
                    } else {
                        Vec::new()
                    };
                    if let Some(path) =
                        self.configuration["frontend_authentication_marker"].as_str()
                    {
                        std::fs::write(path, serde_json::to_vec(&facts).unwrap()).unwrap();
                    }
                    Ok(CallReply::unary(
                        json!({}),
                        serde_json::to_vec(&self.configuration["frontend_authentication_result"])
                            .unwrap(),
                    ))
                }
                "management.handle" => {
                    let results = self.queries(&call, "data_queries").await;
                    Ok(CallReply::unary(
                        json!({"status":200,"content_type":"application/json"}),
                        serde_json::to_vec(&results).unwrap(),
                    ))
                }
                "callback" => {
                    let reply = call
                        .host
                        .call(
                            call.params["method"].as_str().unwrap(),
                            json!({}),
                            Vec::new(),
                        )
                        .await
                        .map_err(|error| error.into_plugin_fault())?;
                    Ok(CallReply::unary(reply.result, reply.payload))
                }
                "partial" => {
                    let fast = Box::pin(call.host.call("fast", json!({}), Vec::new()));
                    let slow = Box::pin(call.host.call("slow", json!({}), Vec::new()));
                    let (first, remaining) = match futures::future::select(fast, slow).await {
                        futures::future::Either::Left(pair)
                        | futures::future::Either::Right(pair) => pair,
                    };
                    first.map_err(|error| error.into_plugin_fault())?;
                    call.host
                        .call("processed", json!({}), Vec::new())
                        .await
                        .map_err(|error| error.into_plugin_fault())?;
                    remaining.await.map_err(|error| error.into_plugin_fault())?;
                    Ok(CallReply::unary(json!("complete"), Vec::new()))
                }
                "busy" => {
                    std::fs::write(call.params["marker"].as_str().unwrap(), "entered").unwrap();
                    // 保持一次 poll 未返回，代表确实占用执行名额的工作
                    std::thread::sleep(std::time::Duration::from_millis(
                        call.params["delay_ms"].as_u64().unwrap(),
                    ));
                    Ok(CallReply::unary(json!("busy complete"), Vec::new()))
                }
                "yield" => {
                    for _ in 0..16 {
                        tokio::task::yield_now().await;
                    }
                    Ok(CallReply::unary(json!("yielded"), Vec::new()))
                }
                "drop_callback" => {
                    let mut pending = Box::pin(call.host.call("slow", json!({}), Vec::new()));
                    assert!(futures::poll!(&mut pending).is_pending());
                    drop(pending);
                    Ok(CallReply::unary(json!("dropped"), Vec::new()))
                }
                "echo" => Ok(CallReply::unary(call.params, call.payload)),
                _ => Err(PluginFault::new(
                    ErrorCode::InvalidInput,
                    "unknown fixture method",
                )),
            }
        })
    }
}
