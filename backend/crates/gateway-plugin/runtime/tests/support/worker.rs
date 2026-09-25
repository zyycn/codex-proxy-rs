//! 真实子进程协议对端；只依赖公开 SDK，Cargo 为集成测试构建此辅助二进制。

use std::{
    collections::BTreeMap,
    io::Write as _,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use gateway_plugin_sdk::{
    ErrorCode, Frame, Message, PluginFault,
    client::{read_frame, write_frame},
};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{Mutex, Notify, mpsc, oneshot};

type CallbackResult = Result<(Value, Vec<u8>), PluginFault>;
struct PendingCallback {
    parent: u64,
    response: Option<oneshot::Sender<CallbackResult>>,
}

fn record_startup(configuration: &Value) -> usize {
    let Some(path) = configuration.get("startup_marker").and_then(Value::as_str) else {
        return 0;
    };
    let startup = std::fs::read_to_string(path)
        .map(|content| content.lines().count())
        .unwrap_or_default();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    serde_json::to_writer(&mut file, &json!({"startup":startup + 1})).unwrap();
    file.write_all(b"\n").unwrap();
    startup
}

#[derive(Default)]
struct Credits {
    available: Mutex<(u64, u64)>,
    changed: Notify,
}

impl Credits {
    async fn take(&self, bytes: u64) {
        loop {
            let changed = self.changed.notified();
            {
                let mut available = self.available.lock().await;
                if available.0 >= bytes && available.1 > 0 {
                    available.0 -= bytes;
                    available.1 -= 1;
                    return;
                }
            }
            changed.await;
        }
    }

    async fn grant(&self, bytes: u32, frames: u32) {
        let mut available = self.available.lock().await;
        available.0 += u64::from(bytes);
        available.1 += u64::from(frames);
        self.changed.notify_one();
    }
}

struct Peer {
    registration: gateway_plugin_sdk::call::registration::Registration,
    configuration: Value,
    output: mpsc::Sender<Frame>,
    callbacks: Mutex<BTreeMap<u64, PendingCallback>>,
    next_callback: AtomicU64,
    streams: Mutex<BTreeMap<u64, Arc<Credits>>>,
}

impl Peer {
    async fn log_fixture(&self, id: u64) {
        let Some(entries) = self
            .configuration
            .get("log_entries")
            .and_then(Value::as_array)
        else {
            return;
        };
        let mut results = Vec::new();
        for entry in entries {
            if let Some(delay) = entry.get("delay_ms").and_then(Value::as_u64) {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            for _ in 0..entry.get("repeat").and_then(Value::as_u64).unwrap_or(1) {
                let payload = if entry["with_payload"] == true {
                    vec![1]
                } else {
                    vec![]
                };
                results.push(
                    match self
                        .callback_payload(id, "host.log", entry["params"].clone(), payload)
                        .await
                    {
                        Ok((result, _)) => result,
                        Err(error) => json!({"error":error.code}),
                    },
                );
            }
        }
        self.append_observation_marker("log_marker", &json!(results));
    }

    fn append_observation_marker(&self, field: &str, value: &Value) {
        let Some(path) = self.configuration.get(field).and_then(Value::as_str) else {
            return;
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        let mut record = serde_json::to_vec(value).unwrap();
        record.push(b'\n');
        file.write_all(&record).unwrap();
    }

    async fn callback(&self, parent: u64, method: &str, params: Value) -> CallbackResult {
        self.callback_payload(parent, method, params, vec![]).await
    }

    async fn callback_payload(
        &self,
        parent: u64,
        method: &str,
        params: Value,
        payload: Vec<u8>,
    ) -> CallbackResult {
        let (response, received) = oneshot::channel();
        {
            let mut callbacks = self.callbacks.lock().await;
            let id = self.next_callback.fetch_add(2, Ordering::Relaxed);
            callbacks.insert(
                id,
                PendingCallback {
                    parent,
                    response: Some(response),
                },
            );
            self.send(
                Message::Callback {
                    id,
                    parent_id: parent,
                    method: method.into(),
                    params,
                },
                payload,
            )
            .await;
        }
        received.await.unwrap()
    }

    async fn run_nested_model_fixture(
        &self,
        parent: u64,
        fixture_field: &str,
        marker_field: &str,
    ) -> Result<(), PluginFault> {
        let Some(fixture) = self.configuration.get(fixture_field) else {
            return Ok(());
        };
        if fixture["stream"] == true {
            let (result, payload) = self
                .callback_payload(
                    parent,
                    "host.model.execute_stream",
                    fixture["request"].clone(),
                    serde_json::to_vec(&fixture["body"]).unwrap(),
                )
                .await?;
            assert!(payload.is_empty());
            let stream: gateway_plugin_sdk::call::host::ModelStreamResult =
                serde_json::from_value(result).unwrap();
            self.append_observation_marker(
                "nested_stream_trace_marker",
                &json!({"phase":"opened","request_id":stream.request_id.as_str()}),
            );
            let mut event_count = 0_u64;
            loop {
                self.append_observation_marker(
                    "nested_stream_trace_marker",
                    &json!({"phase":"read_started"}),
                );
                let (result, payload) = self
                    .callback(
                        parent,
                        "host.model.stream_read",
                        json!({"stream":stream.stream.as_str(),"maximum_bytes":65536}),
                    )
                    .await?;
                let result: gateway_plugin_sdk::call::host::ModelStreamReadResult =
                    serde_json::from_value(result).unwrap();
                let events = if result.end {
                    assert!(payload.is_empty());
                    gateway_plugin_sdk::call::host::ModelEventBatch { events: Vec::new() }
                } else {
                    gateway_plugin_sdk::call::host::ModelEventBatch::decode(&payload).unwrap()
                };
                assert_eq!(usize::try_from(result.events).unwrap(), events.events.len());
                event_count += u64::from(result.events);
                self.append_observation_marker(
                    "nested_stream_trace_marker",
                    &json!({
                        "phase":"read_completed",
                        "events":result.events,
                        "end":result.end,
                        "batch":serde_json::to_value(&events.events).unwrap()
                    }),
                );
                if result.end {
                    self.append_observation_marker(
                        marker_field,
                        &json!({
                            "request_id":stream.request_id.as_str(), "events":event_count,
                        }),
                    );
                    return Ok(());
                }
            }
        }
        let callback = self
            .callback_payload(
                parent,
                "host.model.execute",
                fixture["request"].clone(),
                serde_json::to_vec(&fixture["body"]).unwrap(),
            )
            .await;
        match callback {
            Ok((result, callback_payload)) => {
                let result: gateway_plugin_sdk::call::host::ModelExecuteResult =
                    serde_json::from_value(result).unwrap();
                let events =
                    gateway_plugin_sdk::call::host::ModelEventBatch::decode(&callback_payload)
                        .unwrap();
                assert_eq!(usize::try_from(result.events).unwrap(), events.events.len());
                self.append_observation_marker(
                    marker_field,
                    &json!({"request_id":result.request_id,"events":result.events}),
                );
                Ok(())
            }
            Err(error) if fixture["expect_error"] == true => {
                self.append_observation_marker(marker_field, &json!({"error":error.code}));
                Ok(())
            }
            Err(error) => {
                self.append_observation_marker(marker_field, &json!({"error":error.code}));
                Err(error)
            }
        }
    }

    async fn send(&self, message: Message, payload: Vec<u8>) {
        self.output.send(Frame { message, payload }).await.unwrap();
    }

    async fn respond(&self, id: u64, method: String, params: Value, payload: Vec<u8>) {
        if self.configuration["log_method"].as_str() == Some(method.as_str()) {
            self.log_fixture(id).await;
        }
        if method.starts_with("stream") {
            self.send(
                Message::Result {
                    id,
                    result: json!({"stream":true}),
                },
                vec![],
            )
            .await;
            let credits = self.streams.lock().await.get(&id).unwrap().clone();
            for sequence in 0..128_u64 {
                credits.take(8192).await;
                let payload = if method == "stream_overflow" {
                    vec![b'x'; 256 * 1024 + 1]
                } else {
                    vec![sequence as u8; 8192]
                };
                let sequence = sequence + u64::from(method == "stream_sequence");
                self.send(Message::Stream { id, sequence }, payload).await;
            }
            self.send(Message::End { id, error: None }, vec![]).await;
            return;
        }
        match method.as_str() {
            "policy.retry_decision" => {
                self.append_observation_marker("retry_marker", &params);
                if let Some(delay) = self.configuration["retry_delay_ms"].as_u64() {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                let result = self
                    .configuration
                    .get("retry_decision")
                    .cloned()
                    .unwrap_or_else(|| json!({"decision":"delegate"}));
                self.send(Message::Result { id, result }, vec![]).await;
                return;
            }
            "frontend_auth.identifier" => {
                self.send(
                    Message::Result {
                        id,
                        result: serde_json::to_value(
                            gateway_plugin_sdk::call::frontend_authentication::FrontendAuthenticationIdentifier {
                                identifier: self
                                    .configuration
                                    .get("frontend_authentication_identifier")
                                    .and_then(Value::as_str)
                                    .unwrap_or("fixture-auth")
                                    .to_owned(),
                            },
                        )
                        .unwrap(),
                    },
                    vec![],
                )
                .await;
                return;
            }
            "frontend_auth.authenticate" => {
                assert_eq!(params, json!({}));
                let input: gateway_plugin_sdk::call::frontend_authentication::FrontendAuthenticationRequest =
                    serde_json::from_slice(&payload).unwrap();
                if let Some(expected) = self
                    .configuration
                    .get("expected_frontend_authorization")
                    .and_then(Value::as_str)
                {
                    assert_eq!(input.authorization, expected);
                }
                self.append_observation_marker(
                    "frontend_authentication_marker",
                    &json!({"called":true}),
                );
                let result = self
                    .configuration
                    .get("frontend_authentication_result")
                    .cloned()
                    .unwrap_or_else(|| json!({"outcome":"not_matched"}));
                self.send(
                    Message::Result {
                        id,
                        result: json!({}),
                    },
                    serde_json::to_vec(&result).unwrap(),
                )
                .await;
                return;
            }
            "model_catalog.register" => {
                self.send(
                    Message::Result {
                        id,
                        result: json!({}),
                    },
                    serde_json::to_vec(&self.configuration["model_catalog"]).unwrap(),
                )
                .await;
                return;
            }
            "management.register" => {
                self.send(
                    Message::Result {
                        id,
                        result: json!({}),
                    },
                    serde_json::to_vec(&self.configuration["management_registration"]).unwrap(),
                )
                .await;
                return;
            }
            "management.handle" => {
                let request: gateway_plugin_sdk::call::management::ManagementRequest =
                    serde_json::from_value(params).unwrap();
                self.append_observation_marker(
                    "management_marker",
                    &json!({"method":request.method,"path":request.path}),
                );
                if let Some(queries) = self.configuration["data_queries"].as_array() {
                    let mut results = vec![];
                    for query in queries {
                        let result = self
                            .callback_payload(
                                id,
                                query["method"].as_str().unwrap(),
                                json!({}),
                                serde_json::to_vec(&query["query"]).unwrap(),
                            )
                            .await;
                        results.push(match result {
                            Ok((metadata, payload)) => {
                                assert_eq!(metadata, json!({}));
                                serde_json::from_slice::<Value>(&payload).unwrap()
                            }
                            Err(error) => json!({"error":error.code}),
                        });
                    }
                    self.send(
                        Message::Result {
                            id,
                            result: json!({"status":200,"content_type":"application/json"}),
                        },
                        serde_json::to_vec(&results).unwrap(),
                    )
                    .await;
                    return;
                }
                if let Err(error) = self
                    .run_nested_model_fixture(
                        id,
                        "management_nested_model_fixture",
                        "management_nested_model_marker",
                    )
                    .await
                {
                    self.send(Message::Error { id, error }, vec![]).await;
                    return;
                }
                let response = self
                    .configuration
                    .get("management_response")
                    .cloned()
                    .unwrap_or_else(
                        || json!({"status":200,"content_type":"application/octet-stream"}),
                    );
                self.send(
                    Message::Result {
                        id,
                        result: response,
                    },
                    payload,
                )
                .await;
                return;
            }
            "management.callback" => {
                for method in [
                    "host.auth.list",
                    "host.auth.save",
                    "host.http.do",
                    "host.state.get",
                    "host.log",
                ] {
                    assert!(
                        matches!(self.callback(id, method, json!({})).await, Err(error) if error.code == ErrorCode::PermissionDenied)
                    );
                }
                self.send(
                    Message::Result {
                        id,
                        result: json!({"status":200,"content_type":"text/plain"}),
                    },
                    b"callback received".to_vec(),
                )
                .await;
                return;
            }
            "command_line.register" => {
                if self.configuration["command_registration_probe"] == true {
                    for method in ["host.http.do", "host.auth.save", "host.state.put"] {
                        let reply = self.callback(id, method, json!({})).await;
                        assert!(
                            matches!(reply, Err(ref error) if error.code == ErrorCode::PermissionDenied)
                        );
                    }
                }
                self.send(
                    Message::Result {
                        id,
                        result: json!({}),
                    },
                    serde_json::to_vec(&self.configuration["command_registration"]).unwrap(),
                )
                .await;
                return;
            }
            "command_line.execute" => {
                let invocation: gateway_plugin_sdk::call::management::CommandInvocation =
                    serde_json::from_slice(&payload).unwrap();
                self.append_observation_marker(
                    "command_marker",
                    &json!({"name":invocation.name,"pid":std::process::id()}),
                );
                if self.configuration["command_crash"] == true {
                    std::process::exit(29);
                }
                if self.configuration["command_wait"] == true {
                    std::future::pending::<()>().await;
                }
                if let Err(error) = self
                    .run_nested_model_fixture(
                        id,
                        "command_nested_model_fixture",
                        "command_nested_model_marker",
                    )
                    .await
                {
                    self.send(Message::Error { id, error }, vec![]).await;
                    return;
                }
                if self.configuration["command_error_after_nested_model"].as_str()
                    == Some(invocation.name.as_str())
                {
                    self.send(
                        Message::Error {
                            id,
                            error: PluginFault::new(
                                ErrorCode::Fault,
                                "synthetic failure after nested model",
                            ),
                        },
                        vec![],
                    )
                    .await;
                    return;
                }
                let result = if self.configuration["command_echo"] == true {
                    json!({"stdout":serde_json::to_string(&invocation).unwrap(),"stderr":"typed command\n","exit_code":0})
                } else {
                    self.configuration["command_result"].clone()
                };
                self.send(
                    Message::Result {
                        id,
                        result: json!({}),
                    },
                    serde_json::to_vec(&result).unwrap(),
                )
                .await;
                return;
            }
            "policy.route_model" => {
                let request: gateway_plugin_sdk::call::policy::ModelRouteRequest =
                    serde_json::from_value(params).unwrap();
                self.append_observation_marker(
                    "route_marker",
                    &json!({
                        "request": request,
                        "body": String::from_utf8_lossy(&payload),
                    }),
                );
                if let Some(fixture) = self.configuration.get("affinity_fixture") {
                    let callback = self
                        .callback(id, "host.affinity.lookup", fixture["request"].clone())
                        .await;
                    match callback {
                        Ok((result, callback_payload)) if callback_payload.is_empty() => {
                            self.append_observation_marker(
                                "affinity_marker",
                                &json!({"result":result}),
                            );
                        }
                        Ok(_) => panic!("affinity callback returned an unexpected payload"),
                        Err(error) => {
                            self.send(Message::Error { id, error }, vec![]).await;
                            return;
                        }
                    }
                }
                if let Some(fixture) = self.configuration.get("nested_model_fixture") {
                    let callback = self
                        .callback_payload(
                            id,
                            "host.model.execute",
                            fixture["request"].clone(),
                            serde_json::to_vec(&fixture["body"]).unwrap(),
                        )
                        .await;
                    match callback {
                        Ok((result, callback_payload)) => {
                            let result: gateway_plugin_sdk::call::host::ModelExecuteResult =
                                serde_json::from_value(result).unwrap();
                            let events = gateway_plugin_sdk::call::host::ModelEventBatch::decode(
                                &callback_payload,
                            )
                            .unwrap();
                            assert_eq!(
                                usize::try_from(result.events).unwrap(),
                                events.events.len()
                            );
                            self.append_observation_marker(
                                "nested_model_marker",
                                &json!({
                                    "request_id":result.request_id,
                                    "events":result.events,
                                }),
                            );
                        }
                        Err(error) if fixture["expect_error"] == true => {
                            self.append_observation_marker(
                                "nested_model_marker",
                                &json!({"error":error.code}),
                            );
                        }
                        Err(error) => {
                            self.append_observation_marker(
                                "nested_model_marker",
                                &json!({"error":error.code}),
                            );
                            self.send(Message::Error { id, error }, vec![]).await;
                            return;
                        }
                    }
                }
                if let Some(delay) = self.configuration["route_delay_ms"].as_u64() {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                if let Some(url) = self.configuration["route_http_url"].as_str() {
                    self.callback(id, "host.http.do", json!({"method":"GET", "url":url}))
                        .await
                        .unwrap();
                }
                if self.configuration["route_fault"] == true {
                    self.send(
                        Message::Error {
                            id,
                            error: PluginFault::new(ErrorCode::Fault, "fixture route failure"),
                        },
                        vec![],
                    )
                    .await;
                    return;
                }
                let result = self
                    .configuration
                    .get("route_decision")
                    .cloned()
                    .unwrap_or_else(|| json!({"decision":"unhandled"}));
                let payload = self.configuration["route_reply_payload"]
                    .as_bool()
                    .unwrap_or(false)
                    .then_some(vec![1])
                    .unwrap_or_default();
                self.send(Message::Result { id, result }, payload).await;
                return;
            }
            "policy.schedule_account" => {
                let request: gateway_plugin_sdk::call::policy::AccountScheduleRequest =
                    serde_json::from_value(params).unwrap();
                self.append_observation_marker("schedule_marker", &json!({"request":request}));
                if let Some(delay) = self.configuration["schedule_delay_ms"].as_u64() {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                if self.configuration["schedule_fault"] == true {
                    self.send(
                        Message::Error {
                            id,
                            error: PluginFault::new(ErrorCode::Fault, "fixture schedule failure"),
                        },
                        vec![],
                    )
                    .await;
                    return;
                }
                let result = self
                    .configuration
                    .get("schedule_pick_index")
                    .and_then(Value::as_u64)
                    .and_then(|index| request.candidates.get(index as usize))
                    .map_or_else(
                        || {
                            self.configuration
                                .get("schedule_decision")
                                .cloned()
                                .unwrap_or_else(|| json!({"decision":"delegate"}))
                        },
                        |candidate| json!({"decision":"pick","account_id":candidate.account_id}),
                    );
                let payload = self.configuration["schedule_reply_payload"]
                    .as_bool()
                    .unwrap_or(false)
                    .then_some(vec![1])
                    .unwrap_or_default();
                self.send(Message::Result { id, result }, payload).await;
                return;
            }
            "middleware.handle" => {
                use gateway_plugin_sdk::call::middleware::{
                    BODY_READ_METHOD, MiddlewareBodyRead, MiddlewareBodyReadResult,
                    MiddlewareNextRequest, MiddlewareNextResponse, MiddlewareRequestBody,
                    MiddlewareRequestHead, MiddlewareResponseBody, MiddlewareResponseHead,
                    NEXT_METHOD,
                };

                let request: MiddlewareRequestHead = serde_json::from_value(params).unwrap();
                self.append_observation_marker(
                    "middleware_marker",
                    &json!({
                        "request_id":request.request_id,
                        "mount":request.mount,
                        "protocol":request.protocol,
                        "headers":request.headers,
                        "body_visible":request.body_visible,
                        "body":String::from_utf8_lossy(&payload),
                    }),
                );
                if let Some(url) = self.configuration["middleware_http_url"].as_str() {
                    self.callback(id, "host.http.do", json!({"method":"GET", "url":url}))
                        .await
                        .unwrap();
                }
                if self.configuration["middleware_fault_before_next"] == true {
                    self.send(
                        Message::Error {
                            id,
                            error: PluginFault::new(ErrorCode::Fault, "fixture middleware failure"),
                        },
                        vec![],
                    )
                    .await;
                    return;
                }
                let (result, callback_payload) = self
                    .callback_payload(
                        id,
                        NEXT_METHOD,
                        serde_json::to_value(MiddlewareNextRequest {
                            protocol: None,
                            header_mutations: Vec::new(),
                            body: MiddlewareRequestBody::Preserve,
                            capabilities: None,
                        })
                        .unwrap(),
                        vec![],
                    )
                    .await
                    .unwrap();
                assert!(callback_payload.is_empty());
                let downstream: MiddlewareNextResponse = serde_json::from_value(result).unwrap();
                if self.configuration["middleware_map_body"] == true {
                    let body = downstream.body.clone().unwrap();
                    let (result, mut source) = self
                        .callback_payload(
                            id,
                            BODY_READ_METHOD,
                            serde_json::to_value(MiddlewareBodyRead {
                                handle: body.handle,
                                maximum_bytes: 64 * 1024,
                            })
                            .unwrap(),
                            vec![],
                        )
                        .await
                        .unwrap();
                    let read: MiddlewareBodyReadResult = serde_json::from_value(result).unwrap();
                    assert!(!read.eof && read.source_id != 0);
                    source.push(b' ');
                    let response = MiddlewareResponseHead {
                        response: Some(downstream.response),
                        protocol: None,
                        status: None,
                        header_mutations: Vec::new(),
                        body: MiddlewareResponseBody::Stream {
                            framing: read.framing,
                        },
                    };
                    self.send(
                        Message::Result {
                            id,
                            result: serde_json::to_value(response).unwrap(),
                        },
                        vec![],
                    )
                    .await;
                    let mut mapped = Vec::with_capacity(14 + source.len());
                    mapped.extend_from_slice(b"GMB1");
                    mapped.push(1); // Only: 一个输出消费完整源 frame。
                    mapped.extend_from_slice(&read.source_id.to_be_bytes());
                    mapped.push(0); // mapped frame 的 terminal 由 Runtime 从源事实恢复。
                    mapped.extend_from_slice(&source);
                    let credits = self.streams.lock().await.get(&id).unwrap().clone();
                    credits.take(mapped.len() as u64).await;
                    self.send(Message::Stream { id, sequence: 0 }, mapped).await;
                    if self.configuration["middleware_chunk_after_terminal"] == true {
                        credits.take(1).await;
                        self.send(Message::Stream { id, sequence: 1 }, vec![0])
                            .await;
                    }
                    let error = self.configuration["middleware_error_after_terminal"]
                        .as_bool()
                        .unwrap_or(false)
                        .then(|| {
                            PluginFault::new(
                                ErrorCode::Upstream,
                                "fixture middleware stream interrupted after terminal frame",
                            )
                        });
                    self.send(Message::End { id, error }, vec![]).await;
                    return;
                }
                let response = MiddlewareResponseHead {
                    response: Some(downstream.response),
                    protocol: None,
                    status: None,
                    header_mutations: Vec::new(),
                    body: MiddlewareResponseBody::PassThrough {
                        body: downstream.body.unwrap(),
                    },
                };
                self.send(
                    Message::Result {
                        id,
                        result: serde_json::to_value(response).unwrap(),
                    },
                    vec![],
                )
                .await;
                self.send(Message::End { id, error: None }, vec![]).await;
                return;
            }
            "websocket.response_event" => {
                let observation: gateway_plugin_sdk::call::observation::ObserveWebSocketResponse =
                    serde_json::from_value(params).unwrap();
                let payload_text = (payload.len() <= 4096)
                    .then(|| std::str::from_utf8(&payload).ok())
                    .flatten();
                let marked = json!({
                    "label": self.configuration.get("observation_label"),
                    "observation": observation,
                    "payload_bytes": payload.len(),
                    "payload_text": payload_text,
                });
                self.append_observation_marker("websocket_observation_started_marker", &marked);
                if let Some(delay) = self
                    .configuration
                    .get("websocket_observation_delay_ms")
                    .and_then(Value::as_u64)
                {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                self.append_observation_marker("websocket_observation_marker", &marked);
                if self.configuration.get("websocket_observation_fault") == Some(&Value::Bool(true))
                {
                    self.send(
                        Message::Error {
                            id,
                            error: PluginFault::new(
                                ErrorCode::Fault,
                                "fixture WebSocket observation failure",
                            ),
                        },
                        vec![],
                    )
                    .await;
                    return;
                }
                self.send(
                    Message::Result {
                        id,
                        result: Value::Null,
                    },
                    vec![],
                )
                .await;
                return;
            }
            "policy.observe_request" => {
                let observation: gateway_plugin_sdk::call::policy::ObserveRequest =
                    serde_json::from_slice(&payload).unwrap();
                let marked = json!({
                    "label": self.configuration.get("observation_label"),
                    "observation": observation,
                });
                self.append_observation_marker("observation_started_marker", &marked);
                if let Some(fixture) = self.configuration.get("state_fixture") {
                    let namespace = fixture["namespace"].as_str().unwrap();
                    let key = fixture["key"].as_str().unwrap();
                    let value = fixture["value"].clone();
                    let put = gateway_plugin_sdk::call::host::StatePutRequest {
                        namespace: namespace.into(),
                        key: key.into(),
                        value: value.clone(),
                        expected_version: None,
                    };
                    let put = match self
                        .callback(id, "host.state.put", serde_json::to_value(put).unwrap())
                        .await
                    {
                        Ok((result, payload)) if payload.is_empty() => serde_json::from_value::<
                            gateway_plugin_sdk::call::host::StatePutResult,
                        >(
                            result
                        )
                        .unwrap(),
                        Ok(_) => panic!("state put returned an unexpected payload"),
                        Err(error) => {
                            self.send(Message::Error { id, error }, vec![]).await;
                            return;
                        }
                    };
                    let get = gateway_plugin_sdk::call::host::StateGetRequest {
                        namespace: namespace.into(),
                        key: key.into(),
                    };
                    let get = match self
                        .callback(id, "host.state.get", serde_json::to_value(get).unwrap())
                        .await
                    {
                        Ok((result, payload)) if payload.is_empty() => serde_json::from_value::<
                            gateway_plugin_sdk::call::host::StateGetResult,
                        >(
                            result
                        )
                        .unwrap(),
                        Ok(_) => panic!("state get returned an unexpected payload"),
                        Err(error) => {
                            self.send(Message::Error { id, error }, vec![]).await;
                            return;
                        }
                    };
                    let record = get.record.unwrap();
                    assert_eq!(record.value, value);
                    assert_eq!(record.version, put.version);
                    if let Some(denied_namespace) =
                        fixture.get("denied_namespace").and_then(Value::as_str)
                    {
                        let denied = self
                            .callback(
                                id,
                                "host.state.get",
                                serde_json::to_value(
                                    gateway_plugin_sdk::call::host::StateGetRequest {
                                        namespace: denied_namespace.into(),
                                        key: key.into(),
                                    },
                                )
                                .unwrap(),
                            )
                            .await;
                        assert!(matches!(
                            denied,
                            Err(error) if error.code == ErrorCode::PermissionDenied
                        ));
                    }
                    let deleted = match self
                        .callback(
                            id,
                            "host.state.delete",
                            serde_json::to_value(
                                gateway_plugin_sdk::call::host::StateDeleteRequest {
                                    namespace: namespace.into(),
                                    key: key.into(),
                                    expected_version: put.version,
                                },
                            )
                            .unwrap(),
                        )
                        .await
                    {
                        Ok((result, payload)) if payload.is_empty() => {
                            serde_json::from_value::<
                                gateway_plugin_sdk::call::host::StateDeleteResult,
                            >(result)
                            .unwrap()
                        }
                        Ok(_) => panic!("state delete returned an unexpected payload"),
                        Err(error) => {
                            self.send(Message::Error { id, error }, vec![]).await;
                            return;
                        }
                    };
                    assert!(deleted.deleted);
                    self.append_observation_marker(
                        "state_marker",
                        &json!({"version": put.version, "schema_version": record.schema_version}),
                    );
                }
                if let Some(delay) = self
                    .configuration
                    .get("observation_delay_ms")
                    .and_then(Value::as_u64)
                {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                self.append_observation_marker("observation_marker", &marked);
                if self.configuration.get("observation_fault") == Some(&Value::Bool(true)) {
                    self.send(
                        Message::Error {
                            id,
                            error: PluginFault::new(
                                ErrorCode::Fault,
                                "fixture observation failure",
                            ),
                        },
                        vec![],
                    )
                    .await;
                    return;
                }
                self.send(
                    Message::Result {
                        id,
                        result: Value::Null,
                    },
                    vec![],
                )
                .await;
                return;
            }
            "slow" => tokio::time::sleep(Duration::from_millis(400)).await,
            "hang" | "hang_uncancellable" => return,
            "crash" => std::process::exit(7),
            "callback" | "callback_method" | "forged_callback" => {
                let callback_method = if method == "callback_method" {
                    params["method"].as_str().unwrap().to_owned()
                } else {
                    "host.log".into()
                };
                // 分配与入队保持同一顺序，避免并发回调制造不合法 ID 序列。
                let mut callbacks = self.callbacks.lock().await;
                let callback = self.next_callback.fetch_add(2, Ordering::Relaxed);
                callbacks.insert(
                    callback,
                    PendingCallback {
                        parent: id,
                        response: None,
                    },
                );
                self.send(
                    Message::Callback {
                        id: callback,
                        parent_id: if method == "forged_callback" {
                            999999
                        } else {
                            id
                        },
                        method: callback_method,
                        params,
                    },
                    payload,
                )
                .await;
                return;
            }
            "deny" => {
                let mut error = PluginFault::new(ErrorCode::Rejected, "policy denied");
                error.http_status = Some(403);
                self.send(Message::Error { id, error }, vec![]).await;
                return;
            }
            "plugin.register" => {
                if self.configuration["exit_during_registration"] == true {
                    std::process::exit(31);
                }
                self.send(
                    Message::Result {
                        id,
                        result: serde_json::to_value(&self.registration).unwrap(),
                    },
                    vec![],
                )
                .await;
                return;
            }
            _ => {}
        }
        self.send(Message::Result { id, result: params }, payload)
            .await;
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut input = tokio::io::stdin();
    let (output, mut frames) = mpsc::channel::<Frame>(64);
    tokio::spawn(async move {
        let mut output = tokio::io::stdout();
        while let Some(frame) = frames.recv().await {
            write_frame(&mut output, &frame).await.unwrap();
        }
    });
    let hello = read_frame(&mut input).await.unwrap();
    let Message::Hello { mut handshake } = hello.message else {
        panic!("expected handshake")
    };
    if let Some(events) = handshake
        .configuration
        .get("execution_events_by_artifact")
        .and_then(Value::as_object)
        .and_then(|events| events.get(&handshake.artifact_sha256))
        .cloned()
    {
        handshake.configuration["execution_events"] = events;
    }
    let startup = record_startup(&handshake.configuration);
    if handshake
        .configuration
        .get("startup")
        .and_then(Value::as_str)
        == Some("fail")
        || handshake.configuration["startup_failures"]
            .as_array()
            .and_then(|failures| failures.get(startup))
            .and_then(Value::as_bool)
            == Some(true)
    {
        if let Some(delay) = handshake.configuration["startup_fail_delay_ms"].as_u64() {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        std::process::exit(9)
    }
    if let Some(delay) = handshake
        .configuration
        .get("startup_delay_ms")
        .and_then(Value::as_u64)
    {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let mut contributes = handshake.contributes.clone();
    if handshake.configuration["registration_mismatch"] == true
        && let Some(declaration) = contributes.values_mut().next()
    {
        declaration.id.push_str(".unexpected");
    }
    let registration = gateway_plugin_sdk::call::registration::Registration { contributes };
    let peer = Arc::new(Peer {
        registration,
        configuration: handshake.configuration,
        output,
        callbacks: Mutex::new(BTreeMap::new()),
        next_callback: AtomicU64::new(2),
        streams: Mutex::new(BTreeMap::new()),
    });
    let exit_after_ready = peer.configuration["exit_after_ready_delays_ms"]
        .as_array()
        .and_then(|delays| delays.get(startup))
        .and_then(Value::as_u64)
        .or_else(|| peer.configuration["exit_after_ready_ms"].as_u64());
    let exit_after_ready_signal = peer.configuration["exit_after_ready_signals"]
        .as_array()
        .and_then(|signals| signals.get(startup))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    peer.send(
        Message::Ready {
            protocol_version: gateway_plugin_sdk::PROTOCOL_VERSION,
            incarnation: handshake.incarnation,
        },
        vec![],
    )
    .await;
    if let Some(delay) = exit_after_ready {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            std::process::exit(7);
        });
    } else if let Some(signal) = exit_after_ready_signal {
        tokio::spawn(async move {
            while !std::path::Path::new(&signal).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            std::process::exit(7);
        });
    }
    let mut tasks = BTreeMap::new();
    let mut uncancellable = std::collections::BTreeSet::new();
    let mut last_call = 0;
    while let Ok(frame) = read_frame(&mut input).await {
        match frame.message {
            Message::Call {
                id,
                method,
                params,
                context,
            } => {
                assert!(id > last_call && !id.is_multiple_of(2) && context.call_id == id);
                last_call = id;
                if matches!(
                    method.as_str(),
                    "malformed_truncated_frame" | "malformed_frame_length"
                ) {
                    let bytes: &[u8] = if method == "malformed_truncated_frame" {
                        // 声明 16 字节元数据，却只写入一个字节后退出。
                        &[0, 0, 0, 16, 0, 0, 0, 0, b'{']
                    } else {
                        // 元数据长度超过公开的 64 KiB 上限，读取端必须在分配前拒绝。
                        &[0, 1, 0, 1, 0, 0, 0, 0]
                    };
                    let mut output = tokio::io::stdout();
                    output.write_all(bytes).await.unwrap();
                    output.flush().await.unwrap();
                    std::process::exit(42);
                }
                tasks.retain(|_, task: &mut tokio::task::JoinHandle<()>| !task.is_finished());
                if method == "hang_uncancellable" {
                    uncancellable.insert(id);
                }
                if method.starts_with("stream") || matches!(method.as_str(), "middleware.handle") {
                    peer.streams
                        .lock()
                        .await
                        .insert(id, Arc::new(Credits::default()));
                }
                let peer = Arc::clone(&peer);
                tasks.insert(
                    id,
                    tokio::spawn(
                        async move { peer.respond(id, method, params, frame.payload).await },
                    ),
                );
            }
            Message::Cancel { id } => {
                if uncancellable.contains(&id) {
                    continue;
                }
                if let Some(task) = tasks.remove(&id) {
                    task.abort();
                    let _ = task.await;
                }
                peer.streams.lock().await.remove(&id);
                peer.callbacks
                    .lock()
                    .await
                    .retain(|_, callback| callback.parent != id);
                peer.append_observation_marker(
                    "execution_cancelled_marker",
                    &json!({"call_id":id}),
                );
                peer.send(Message::Cancelled { id }, vec![]).await;
            }
            Message::Credit { id, bytes, frames } => {
                if let Some(credits) = peer.streams.lock().await.get(&id) {
                    credits.grant(bytes, frames).await;
                }
            }
            Message::Result { id, result } => {
                let parent = peer.callbacks.lock().await.remove(&id);
                if let Some(callback) = parent {
                    if let Some(response) = callback.response {
                        let _ = response.send(Ok((result, frame.payload)));
                    } else {
                        peer.send(
                            Message::Result {
                                id: callback.parent,
                                result,
                            },
                            frame.payload,
                        )
                        .await;
                    }
                }
            }
            Message::Error { id, error } => {
                let parent = peer.callbacks.lock().await.remove(&id);
                if let Some(callback) = parent {
                    if let Some(response) = callback.response {
                        let _ = response.send(Err(error));
                    } else {
                        peer.send(
                            Message::Error {
                                id: callback.parent,
                                error,
                            },
                            frame.payload,
                        )
                        .await;
                    }
                }
            }
            Message::Shutdown => break,
            _ => {}
        }
    }
}
