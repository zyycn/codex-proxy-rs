//! 中间件测试插件子进程入口及异常退出清理辅助

mod http_resources;

use gateway_plugin_sdk::{
    Capability, ContributionDeclaration, Contributions, Stage,
    client::{MiddlewarePlugin, PluginSession, RequestCall, SessionConfig},
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session = PluginSession::accept(
        tokio::io::stdin(),
        tokio::io::stdout(),
        SessionConfig::default(),
    )
    .await?;
    let mode = session.handshake().configuration["mode"]
        .as_str()
        .unwrap_or("map")
        .to_owned();
    let configuration = session.handshake().configuration.clone();
    if mode == "mixed_mounts" {
        let contributes = Contributions::from([(
            Capability::Middleware,
            ContributionDeclaration {
                id: "test.example.middleware".into(),
                version: 3,
                stages: vec![
                    Stage::Http,
                    Stage::WebSocket,
                    Stage::Service,
                    Stage::Request,
                    Stage::Attempt,
                ],
                input_formats: vec![
                    "http".into(),
                    "websocket".into(),
                    "service".into(),
                    "openai".into(),
                ],
                output_formats: vec![
                    "http".into(),
                    "websocket".into(),
                    "service".into(),
                    "openai".into(),
                ],
            },
        )]);
        let plugin = MiddlewarePlugin::new(
            &contributes,
            move |call: gateway_plugin_sdk::client::MiddlewareCall| {
                let records = configuration["records"].as_str().unwrap().to_owned();
                async move {
                    use std::io::Write as _;
                    let context = call.context().clone();
                    let record = |phase| {
                        let mut file = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&records)
                            .unwrap();
                        writeln!(file, "{phase}:{:?}:{}", context.stage, context.instance_id)
                            .unwrap();
                    };
                    record("enter");
                    let result = call.forward().await;
                    record("exit");
                    result
                }
            },
        )?;
        session.run(plugin).await?;
        return Ok(());
    }
    let contributes = Contributions::from([(
        Capability::Middleware,
        ContributionDeclaration {
            id: "test.example.middleware".into(),
            version: configuration["middleware_version"]
                .as_u64()
                .unwrap_or(3)
                .try_into()?,
            stages: vec![if configuration["service"] == true {
                Stage::Service
            } else if configuration["http"] == true {
                Stage::Http
            } else if configuration["websocket"] == true {
                Stage::WebSocket
            } else if configuration["attempt"] == true {
                Stage::Attempt
            } else {
                Stage::Request
            }],
            input_formats: vec![if configuration["service"] == true {
                "service".into()
            } else if configuration["http"] == true {
                "http".into()
            } else if configuration["websocket"] == true {
                "websocket".into()
            } else {
                "openai".into()
            }],
            output_formats: vec![if configuration["service"] == true {
                "service".into()
            } else if configuration["http"] == true {
                "http".into()
            } else if configuration["websocket"] == true {
                "websocket".into()
            } else {
                "openai".into()
            }],
        },
    )]);
    if configuration["service"] == true {
        let plugin = MiddlewarePlugin::new(
            &contributes,
            move |call: gateway_plugin_sdk::client::ServiceCall| {
                let mode = mode.clone();
                let records = configuration["records"]
                    .as_str()
                    .map(std::path::PathBuf::from);
                async move {
                    if mode == "onion" {
                        use std::io::Write as _;
                        let records = records.unwrap();
                        let mut event = serde_json::json!({
                            "instance":call.context.instance_id,
                            "operation":call.operation,
                            "request_id":call.request_id,
                            "call_id":call.call_id,
                            "parent_call_id":call.parent_call_id,
                        });
                        let mut record = |phase| {
                            event["phase"] = serde_json::json!(phase);
                            let mut file = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&records)
                                .unwrap();
                            writeln!(file, "{event}").unwrap();
                        };
                        record("enter");
                        let result = call.forward().await;
                        record("exit");
                        return result;
                    }
                    if mode == "fault" {
                        let mut fault = gateway_plugin_sdk::PluginFault::new(
                            gateway_plugin_sdk::ErrorCode::Capacity,
                            "service plugin fixture error",
                        );
                        fault.details = Some(
                            serde_json::json!({"opaque":"unfiltered","empty":"","zero":0,"disabled":false,"missing":null}),
                        );
                        return Err(fault);
                    }
                    if let Some(records) = &records {
                        use std::io::Write as _;
                        let mut file = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(records)
                            .unwrap();
                        writeln!(file, "{}", serde_json::json!({"operation":call.operation,"request_id":call.request_id,"call_id":call.call_id,"parent_call_id":call.parent_call_id})).unwrap();
                    }
                    if mode == "block" {
                        struct Cleanup(std::path::PathBuf);
                        impl Drop for Cleanup {
                            fn drop(&mut self) {
                                std::fs::write(self.0.with_extension("closed"), b"closed").unwrap();
                            }
                        }
                        let _cleanup = Cleanup(records.unwrap());
                        call.cancellation.cancelled().await;
                        return Err(gateway_plugin_sdk::PluginFault::new(
                            gateway_plugin_sdk::ErrorCode::Cancelled,
                            "cancelled",
                        ));
                    }
                    use gateway_plugin_sdk::{call::services::settings, client::ServiceResponse};
                    if call.is::<settings::ApiKeyExists>() && mode == "recover" {
                        return ServiceResponse::from_result::<settings::ApiKeyExists>(Ok(true));
                    }
                    if call.is::<settings::Load>() {
                        let call = call.into_typed::<settings::Load>()?;
                        // 同一插件再次读取服务不会递归调用自身；回调仍经过宿主服务端口
                        let nested = call.host.service::<settings::Load>(()).await.unwrap();
                        let mut output = call.next.run(()).await.unwrap();
                        assert_eq!(output.config_revision, nested.config_revision);
                        if mode == "rewrite" {
                            output.request_interval_ms = 4321;
                        }
                        return ServiceResponse::from_result::<settings::Load>(Ok(output));
                    }
                    if call.is::<settings::Replace>() {
                        let mut call = call.into_typed::<settings::Replace>()?;
                        call.input.1.request_interval_ms = 1234;
                        return ServiceResponse::from_result::<settings::Replace>(
                            call.next.run(call.input).await,
                        );
                    }
                    if call.is::<settings::PreviewClientProfile>() && mode == "recover" {
                        let call = call.into_typed::<settings::PreviewClientProfile>()?;
                        let error = call.next.run(call.input).await.unwrap_err();
                        assert_eq!(error.kind, "invalid");
                        assert!(!error.message.is_empty());
                        return ServiceResponse::from_result::<settings::PreviewClientProfile>(Ok(
                            serde_json::Map::new(),
                        ));
                    }
                    call.forward().await
                }
            },
        )?;
        session.run(plugin).await?;
        return Ok(());
    }
    if configuration["websocket"] == true {
        let plugin = MiddlewarePlugin::new(
            &contributes,
            move |call: gateway_plugin_sdk::client::WebSocketCall| {
                let mode = mode.clone();
                async move {
                    use gateway_plugin_sdk::client::{WebSocketKind, WebSocketMessage};
                    assert!(
                        call.headers
                            .iter()
                            .any(|header| header.name == "authorization"
                                && header.value == b"Bearer fixture-secret")
                    );
                    if mode == "control" {
                        assert_eq!(call.message.payload.collect().await?, b"custom.control");
                        call.sender
                            .send(WebSocketMessage::new(
                                WebSocketKind::Binary,
                                vec![0, 255, 1],
                            ))
                            .await?;
                        return Ok(None);
                    }
                    let mut message = call.message;
                    if mode == "map" {
                        let bytes = message.payload.collect().await?.to_ascii_uppercase();
                        message = WebSocketMessage::new(message.kind, bytes);
                    }
                    let result = call.next.run(message).await?;
                    Ok(result)
                }
            },
        )?;
        session.run(plugin).await?;
        return Ok(());
    }
    if configuration["http"] == true {
        let plugin = MiddlewarePlugin::new(
            &contributes,
            move |mut call: gateway_plugin_sdk::client::HttpCall| {
                let mode = mode.clone();
                async move {
                    use gateway_plugin_sdk::client::{HttpBody, HttpFrame, HttpResponse};
                    if mode == "upload_resources" {
                        http_resources::check_upload_lifecycle(&call.host).await?;
                        return call.next.run(call.request).await;
                    }
                    if mode == "sequential_uploads" {
                        for index in 0_u32..40 {
                            let expected = index.to_be_bytes().to_vec();
                            let request = gateway_plugin_sdk::client::HttpRequest {
                                settings: call.request.settings.clone(),
                                method: "POST".into(),
                                uri: "/child".into(),
                                version: call.request.version,
                                headers: Vec::new(),
                                timeout_ms: call.request.timeout_ms,
                                body: HttpBody::from_bytes(expected.clone())
                                    .map_frames(|frame| Ok(Some(frame))),
                            };
                            let mut response = call.host.dispatch_http(request).await?;
                            let mut bytes = Vec::new();
                            while let Some(frame) = response.body.read().await? {
                                match frame {
                                    HttpFrame::Data(chunk) => bytes.extend(chunk),
                                    HttpFrame::Trailers(_) => panic!("unexpected trailers"),
                                }
                            }
                            assert_eq!(bytes, expected);
                            assert!(response.body.read().await?.is_none());
                            response.body.close().await?;
                        }
                        return call.next.run(call.request).await;
                    }
                    if mode == "close_response_body" || mode == "read_response_body" {
                        let mut response = call.next.run(call.request).await?;
                        if mode == "read_response_body" {
                            assert!(
                                matches!(response.body.read().await?, Some(HttpFrame::Data(bytes)) if bytes == b"original")
                            );
                            assert!(response.body.read().await?.is_none());
                            return Ok(response);
                        }
                        response.body.close().await?;
                        response.body.close().await?;
                        response.body = HttpBody::from_bytes(b"replacement".to_vec());
                        return Ok(response);
                    }
                    if mode == "settings" {
                        if call.context.instance_id == "dispatch-a" {
                            assert_eq!(call.request.timeout_ms, Some(60_000));
                            call.request.timeout_ms = Some(90_000);
                            assert!(call.request.settings["min_codex_cli_version"].is_null());
                            call.request.settings["min_codex_cli_version"] =
                                serde_json::json!("0.40.0");
                            call.request.settings["request_interval_ms"] = serde_json::json!(0);
                        } else {
                            assert_eq!(call.request.timeout_ms, Some(90_000));
                            assert_eq!(
                                call.settings_sources["http_timeout"]["change"]["instance_id"],
                                "dispatch-a"
                            );
                            call.request.timeout_ms = None;
                            assert_eq!(call.request.settings["min_codex_cli_version"], "0.40.0");
                            assert_eq!(
                                call.settings_sources["overrides"]["min_codex_cli_version"]["instance_id"],
                                "dispatch-a"
                            );
                            call.request.settings["min_codex_cli_version"] =
                                serde_json::Value::Null;
                            call.request.settings["responses_max_decompressed_body_bytes"] =
                                serde_json::json!(8192);
                        }
                        return call.host.dispatch_http(call.request).await;
                    }
                    if mode == "callbacks" {
                        let host = call.host.clone();
                        return call.upgrade(vec!["test-session".into()], |mut session| async move {
                            use gateway_plugin_sdk::client::{WebSocketKind, WebSocketMessage, SessionError};
                            session.sender.send(WebSocketMessage::new(WebSocketKind::Text, b"ready".to_vec())).await?;
                            while let Some(message) = session.receive().await? {
                                let command: serde_json::Value = serde_json::from_slice(&message.payload.collect().await?).unwrap();
                                let payload = command.get("body").map(|body| serde_json::to_vec(body).unwrap()).unwrap_or_default();
                                let result = host.call(command["method"].as_str().unwrap(), command["params"].clone(), payload).await.map_err(SessionError::into_plugin_fault)?;
                                let reply = serde_json::json!({"result":result.result,"payload_bytes":result.payload.len()});
                                session.sender.send(WebSocketMessage::new(WebSocketKind::Text, serde_json::to_vec(&reply).unwrap())).await?;
                            }
                            Ok(())
                        }).await;
                    }
                    if mode == "upgrade" {
                        return call.upgrade(vec!["test-session".into()], |mut session| async move {
                            use gateway_plugin_sdk::client::{WebSocketKind, WebSocketMessage};
                            let sender = session.sender.clone();
                            for index in 0..3 {
                                let receive = session.receive();
                                tokio::pin!(receive);
                                tokio::select! {
                                    message = &mut receive => panic!("receive must wait before greeting: {}", message.is_ok()),
                                    () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {},
                                }
                                if index == 0 {
                                    sender.send(WebSocketMessage::new(WebSocketKind::Text, b"ready".to_vec())).await?;
                                }
                                // 模拟模型任务先完成导致 select 丢弃 receive；下一次等待必须继续取同一条消息
                            }
                            if let Some(message) = session.receive().await? { sender.send(message).await?; }
                            for index in 0..32 {
                                let Some(mut message) = session.receive().await? else { break; };
                                let payload = if index % 2 == 0 {
                                    message.payload.collect().await?
                                } else {
                                    let bytes = message.payload.read().await?.unwrap_or_default();
                                    message.payload.close().await?;
                                    bytes
                                };
                                sender.send(WebSocketMessage::new(message.kind, payload)).await?;
                            }
                            session.close(Some(1000), "done".into()).await
                        }).await;
                    }
                    if mode == "dispatch" || mode == "dispatch_upload" {
                        call.request.headers.push(
                            gateway_plugin_sdk::call::middleware::MiddlewareHeader {
                                name: "x-dispatch-instance".into(),
                                value: call.context.instance_id.as_bytes().to_vec(),
                            },
                        );
                        if mode == "dispatch_upload" {
                            call.request.body = call.request.body.map_frames(|frame| {
                                Ok(Some(match frame {
                                    HttpFrame::Data(bytes) => {
                                        HttpFrame::Data(bytes.to_ascii_uppercase())
                                    }
                                    HttpFrame::Trailers(headers) => HttpFrame::Trailers(headers),
                                }))
                            });
                        }
                        return call.host.dispatch_http(call.request).await;
                    }
                    if mode == "identity" {
                        return call.next.run(call.request).await;
                    }
                    if mode == "short" {
                        return Ok(HttpResponse::new(
                            418,
                            HttpBody::from_bytes(b"plugin route".to_vec()),
                        ));
                    }
                    assert!(
                        call.request
                            .headers
                            .iter()
                            .any(|header| header.name == "authorization"
                                && header.value == b"Bearer fixture-secret")
                    );
                    call.request.uri = "/rewritten?encoded=%2F".into();
                    call.request.method = "PATCH".into();
                    call.request.timeout_ms = None;
                    if mode == "upload" {
                        call.request.body = call.request.body.map_frames(|frame| {
                            Ok(Some(match frame {
                                HttpFrame::Data(bytes) => {
                                    HttpFrame::Data(bytes.to_ascii_uppercase())
                                }
                                HttpFrame::Trailers(headers) => HttpFrame::Trailers(headers),
                            }))
                        });
                    }
                    let mut response = call.next.run(call.request).await?;
                    response
                        .headers
                        .push(gateway_plugin_sdk::call::middleware::MiddlewareHeader {
                            name: "x-http-plugin".into(),
                            value: b"active".to_vec(),
                        });
                    if mode == "map" {
                        response.body = response.body.map_frames(|frame| {
                            Ok(Some(match frame {
                                HttpFrame::Data(bytes) => {
                                    HttpFrame::Data(bytes.to_ascii_uppercase())
                                }
                                HttpFrame::Trailers(headers) => HttpFrame::Trailers(headers),
                            }))
                        });
                    }
                    Ok(response)
                }
            },
        )?;
        session.run(plugin).await?;
        return Ok(());
    }
    let plugin = MiddlewarePlugin::new(&contributes, move |mut call: RequestCall| {
        let mode = mode.clone();
        let configuration = configuration.clone();
        async move {
            if mode == "declare" {
                call.request
                    .replace_body(serde_json::to_vec(&configuration["body"]).unwrap());
                call.request.declare_capabilities(
                    serde_json::from_value(configuration["capabilities"].clone()).unwrap(),
                );
            }
            if mode == "credentials" {
                assert!(
                    call.request
                        .head
                        .headers
                        .iter()
                        .any(|header| header.name == "authorization"
                            && header.value == b"Bearer private")
                );
                assert!(
                    call.request
                        .head
                        .headers
                        .iter()
                        .any(|header| header.name == "connection")
                );
                call.request.remove_header("authorization");
                call.request
                    .append_header("authorization", b"Bearer rewritten".to_vec());
            }
            if mode == "headers" {
                call.request
                    .append_header("x-sdk-request", b"active".to_vec());
            }
            if mode == "settings" {
                assert_eq!(
                    call.request.head.settings,
                    configuration["expected_settings"]
                );
                if let Some(settings) = configuration.get("settings") {
                    call.request.head.settings = settings.clone();
                }
            }
            if mode == "inspect" && !call.request.body.is_empty() {
                // 读取和解析只能作用于副本，不能把解析结果写回未改写的正文
                assert!(serde_json::from_slice::<serde_json::Value>(&call.request.body).is_ok());
            }
            if mode == "rejected" {
                let mut fault = gateway_plugin_sdk::PluginFault::new(
                    gateway_plugin_sdk::ErrorCode::Rejected,
                    "plugin rejection details",
                );
                fault.details = Some(serde_json::json!({"reason":"fixture","disabled":false}));
                return Err(fault);
            }
            if mode == "downstream_error" {
                let error = match call.next.run(call.request).await {
                    Ok(_) => panic!("expected downstream failure"),
                    Err(error) => error,
                };
                let fault = &error;
                assert_eq!(
                    fault.message,
                    configuration["expected_message"].as_str().unwrap()
                );
                if configuration["remote_error"] == true {
                    assert_eq!(
                        fault.details.as_ref().unwrap(),
                        &configuration["expected_details"]
                    );
                    assert_eq!(fault.code, gateway_plugin_sdk::ErrorCode::Rejected);
                } else {
                    assert_eq!(
                        fault.details.as_ref().unwrap()["provider"]["response"]["body"],
                        configuration["expected_body"]
                    );
                    assert_eq!(fault.http_status, Some(429));
                    assert_eq!(fault.send_state, gateway_plugin_sdk::SendState::Sent);
                }
                return Err(error);
            }
            let mut response = call.next.run(call.request).await?;
            match mode.as_str() {
                "facts" => {
                    let path = configuration["facts_marker"].as_str().unwrap().to_owned();
                    std::fs::write(
                        &path,
                        format!("{}\n", serde_json::to_string(&response.metadata).unwrap()),
                    )
                    .unwrap();
                    // 修改快照不能替换原执行的账号或费用
                    response.metadata.as_mut().unwrap().provider_account_id =
                        "changed-snapshot".into();
                    response.body = response.body.with_facts()?.map_frames(move |mut frame| {
                        use std::io::Write;
                        let facts = frame.facts.as_mut().unwrap();
                        let mut file = std::fs::OpenOptions::new()
                            .append(true)
                            .open(&path)
                            .unwrap();
                        writeln!(file, "{}", serde_json::to_string(facts).unwrap()).unwrap();
                        facts.host.as_mut().unwrap().costs.clear();
                        Ok(vec![frame])
                    })?;
                }
                "passthrough" | "declare" | "settings" => {}
                "inspect" => {
                    response.body = response.body.inspect_frames(|frame| {
                        assert!(!frame.payload.is_empty());
                    })?;
                }
                "credentials" => {
                    assert!(
                        response
                            .headers
                            .iter()
                            .any(|header| header.name == "set-cookie"
                                && header.value == b"test-hidden=kept")
                    );
                    response.remove_header("set-cookie");
                    response.append_header("set-cookie", b"rewritten=true".to_vec());
                }
                "headers" => response.append_header("x-sdk-response", b"active".to_vec()),
                _ => {
                    response.append_header("x-sdk-plugin", b"active".to_vec());
                    response.body = response.body.map_frames(|mut frame| {
                        frame.payload.push(b' ');
                        Ok(vec![frame])
                    })?;
                }
            }
            Ok(response)
        }
    })?;
    session.run(plugin).await?;
    Ok(())
}
