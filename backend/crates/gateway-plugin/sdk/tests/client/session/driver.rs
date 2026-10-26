//! 会话调用分派、并发帧读取与处理器响应边界测试

use super::*;
use gateway_plugin_sdk::CallBudgets;

#[tokio::test(start_paused = true)]
async fn declared_long_budgets_survive_callbacks_and_expose_remaining_time() {
    for budget_ms in [600_000, 1_800_000] {
        let (host, plugin) = tokio::io::duplex(16_384);
        let (plugin_reader, plugin_writer) = tokio::io::split(plugin);
        let task = tokio::spawn(async move {
            let mut session =
                PluginSession::accept(plugin_reader, plugin_writer, SessionConfig::default())
                    .await?;
            let declared = session.handshake().configuration["maintenance_ms"]
                .as_u64()
                .unwrap();
            session.set_call_budgets(CallBudgets::from([(Stage::Maintenance, declared)]))?;
            session.run(TestHandler::default()).await
        });
        let (reader, writer) = tokio::io::split(host);
        let mut host = HostPeer { reader, writer };
        let mut hello = handshake();
        hello.configuration = json!({"maintenance_ms": budget_ms});
        send_control(&mut host, Message::Hello { handshake: hello }).await;
        let Message::Ready { call_budgets, .. } = receive(&mut host).await.message else {
            panic!("expected Ready")
        };
        assert_eq!(call_budgets[&Stage::Maintenance], budget_ms);
        let mut call_context = context(1, Duration::from_millis(budget_ms));
        call_context.stage = Stage::Maintenance;
        send_control(
            &mut host,
            Message::Call {
                id: 1,
                method: "budget".into(),
                context: call_context,
                params: json!({}),
            },
        )
        .await;
        let Message::Callback {
            id, parent_id: 1, ..
        } = receive(&mut host).await.message
        else {
            panic!("expected callback")
        };
        tokio::time::advance(Duration::from_secs(125)).await;
        send_control(
            &mut host,
            Message::Result {
                id,
                result: json!({}),
            },
        )
        .await;
        let Message::Result { id: 1, result } = receive(&mut host).await.message else {
            panic!("long call must remain active")
        };
        assert_eq!(result["initial_ms"], budget_ms);
        let remaining = result["remaining_ms"].as_u64().unwrap();
        assert!(remaining <= budget_ms - 125_000 && remaining >= budget_ms - 126_000);
        shutdown(&mut host, task).await;
    }
}

#[tokio::test]
async fn calls_cannot_exceed_the_budget_negotiated_for_their_stage() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    let mut context = context(1, Duration::from_secs(3));
    context.stage = Stage::Retry;
    send_control(
        &mut host,
        Message::Call {
            id: 1,
            method: "echo".into(),
            context,
            params: json!({}),
        },
    )
    .await;
    assert!(matches!(task.await.unwrap(), Err(SessionError::Protocol)));
}

#[test]
fn middleware_plugin_rejects_declarations_without_matching_handlers() {
    let declaration = middleware_contributions()
        .remove(&Capability::Middleware)
        .unwrap();
    let mut invalid = vec![Contributions::new()];
    invalid.push(Contributions::from([(
        Capability::Scheduler,
        declaration.clone(),
    )]));
    invalid.push(Contributions::from([
        (Capability::Middleware, declaration.clone()),
        (Capability::Scheduler, declaration.clone()),
    ]));
    let mut unsupported = declaration.clone();
    unsupported.version = 2;
    invalid.push(Contributions::from([(Capability::Middleware, unsupported)]));
    for stages in [
        vec![],
        vec![Stage::Registration],
        vec![Stage::Request, Stage::Request],
        vec![Stage::Request, Stage::Attempt, Stage::Request],
    ] {
        let mut invalid_stage = declaration.clone();
        invalid_stage.stages = stages;
        invalid.push(Contributions::from([(
            Capability::Middleware,
            invalid_stage,
        )]));
    }
    for contributes in invalid {
        let result = MiddlewarePlugin::new(&contributes, |call: RequestCall| async move {
            call.next.run(call.request).await
        });
        assert!(matches!(result, Err(SessionError::Configuration)));
    }
    let mut both_mounts = declaration;
    both_mounts.stages.push(Stage::Attempt);
    let contributes = Contributions::from([(Capability::Middleware, both_mounts)]);
    assert!(
        MiddlewarePlugin::new(&contributes, |call: RequestCall| async move {
            call.next.run(call.request).await
        })
        .is_ok()
    );
}

#[tokio::test]
async fn middleware_plugin_registers_without_invoking_business_handler() {
    let contributes = middleware_contributions();
    let plugin = MiddlewarePlugin::new(&contributes, |_: RequestCall| async {
        panic!("registration must not invoke middleware")
    })
    .unwrap();
    let (mut host, task) = start_session(plugin).await;
    let mut registration_context = context(1, Duration::from_secs(1));
    registration_context.stage = Stage::Registration;
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Call {
                id: 1,
                method: "plugin.register".into(),
                context: registration_context,
                params: json!({}),
            },
            payload: vec![],
        },
    )
    .await
    .unwrap();
    let reply = receive(&mut host).await;
    let Message::Result { id: 1, result } = reply.message else {
        panic!("expected registration result")
    };
    let registration: gateway_plugin_sdk::call::registration::Registration =
        serde_json::from_value(result).unwrap();
    assert_eq!(registration.contributes, contributes);
    assert!(reply.payload.is_empty());
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn middleware_plugin_rejects_invalid_calls_before_business_dispatch() {
    let plugin = MiddlewarePlugin::new(&middleware_contributions(), |_: RequestCall| async {
        panic!("invalid call must not invoke middleware")
    })
    .unwrap();
    let (mut host, task) = start_session(plugin).await;
    let cases = [
        ("plugin.register", Stage::Request, json!({}), vec![]),
        ("plugin.register", Stage::Registration, json!(null), vec![]),
        (
            "plugin.register",
            Stage::Registration,
            json!({"extra":true}),
            vec![],
        ),
        ("plugin.register", Stage::Registration, json!({}), vec![1]),
        (HANDLE_METHOD, Stage::Attempt, json!({}), vec![]),
        (HANDLE_METHOD, Stage::Request, json!({}), vec![]),
        ("unknown.execute", Stage::Request, json!({}), vec![]),
    ];
    for (index, (method, stage, params, payload)) in cases.into_iter().enumerate() {
        let id = u64::try_from(index).unwrap() * 2 + 1;
        let mut call_context = middleware_context(id);
        call_context.stage = stage;
        write_frame(
            &mut host.writer,
            &Frame {
                message: Message::Call {
                    id,
                    method: method.into(),
                    context: call_context,
                    params,
                },
                payload,
            },
        )
        .await
        .unwrap();
        let reply = receive(&mut host).await;
        let Message::Error {
            id: reply_id,
            error,
        } = reply.message
        else {
            panic!("expected invalid call error")
        };
        assert_eq!(reply_id, id);
        assert_eq!(
            error.code,
            if method == "unknown.execute" {
                ErrorCode::Unsupported
            } else {
                ErrorCode::InvalidInput
            }
        );
    }
    shutdown(&mut host, task).await;
}

#[tokio::test(start_paused = true)]
async fn independent_calls_complete_concurrently() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "slow", json!("slow"), vec![1]).await;
    send_call(&mut host, 3, "echo", json!("fast"), vec![3]).await;

    let fast = receive(&mut host).await;
    let slow = receive(&mut host).await;
    assert!(matches!(
        fast,
        Frame {
            message: Message::Result { id: 3, result },
            payload,
        } if result == json!("fast") && payload == [3]
    ));
    assert!(matches!(
        slow,
        Frame {
            message: Message::Result { id: 1, result },
            payload,
        } if result == json!("slow") && payload == [1]
    ));

    shutdown(&mut host, task).await;
}

#[tokio::test(start_paused = true)]
async fn completed_call_does_not_discard_a_partially_read_next_frame() {
    let next = Frame {
        message: Message::Call {
            id: 3,
            method: "echo".into(),
            context: context(3, Duration::from_secs(1)),
            params: json!({"fragmented": true}),
        },
        payload: vec![0, 255, 13, 10, 128],
    };
    let mut encoded = Vec::new();
    write_frame(&mut encoded, &next).await.unwrap();
    let payload_start = encoded.len() - next.payload.len();
    for split in [2, 6, 10, payload_start + 2] {
        tokio::time::timeout(Duration::from_secs(2), async {
            // 单字节缓冲保证前缀已经进入读取 future，而不是仍滞留在传输队列
            let (mut host, task) = start_session_with_capacity(TestHandler::default(), 1).await;
            send_call(&mut host, 1, "delayed_stream", json!({}), Vec::new()).await;
            send_control(
                &mut host,
                Message::Credit {
                    id: 1,
                    bytes: 1_024,
                    frames: 1,
                },
            )
            .await;
            assert!(matches!(
                receive(&mut host).await.message,
                Message::Result { id: 1, .. }
            ));
            host.writer.write_all(&encoded[..split]).await.unwrap();
            assert!(matches!(
                receive(&mut host).await.message,
                Message::Stream { id: 1, sequence: 0 }
            ));
            assert!(matches!(
                receive(&mut host).await.message,
                Message::End { id: 1, error: None }
            ));
            host.writer.write_all(&encoded[split..]).await.unwrap();
            assert_eq!(
                receive(&mut host).await,
                Frame {
                    message: Message::Result {
                        id: 3,
                        result: json!({"fragmented": true})
                    },
                    payload: next.payload.clone(),
                }
            );
            shutdown(&mut host, task).await;
        })
        .await
        .expect("fragmented frame must survive concurrent call completion");
    }
}

#[tokio::test]
async fn middleware_next_and_body_mapping_reuse_callback_and_credit_flow() {
    let (mut host, task) = start_session(middleware_plugin(true)).await;
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Call {
                id: 1,
                method: HANDLE_METHOD.into(),
                context: middleware_context(1),
                params: serde_json::to_value(middleware_request()).unwrap(),
            },
            payload: b"request".to_vec(),
        },
    )
    .await
    .unwrap();

    let next = receive(&mut host).await;
    let next_id = match next.message {
        Message::Callback {
            id,
            parent_id: 1,
            ref method,
            ref params,
        } if method == NEXT_METHOD => {
            let request: MiddlewareNextRequest = serde_json::from_value(params.clone()).unwrap();
            assert_eq!(request.body, MiddlewareRequestBody::Replace);
            assert!(request.protocol.is_none());
            assert_eq!(request.header_mutations.len(), 2);
            assert!(matches!(
                &request.header_mutations[0],
                MiddlewareHeaderMutation::Remove { name } if name == "x-old"
            ));
            assert!(matches!(
                &request.header_mutations[1],
                MiddlewareHeaderMutation::Append { name, value }
                    if name == "x-direct" && value == b"request"
            ));
            id
        }
        message => panic!("unexpected middleware next frame: {message:?}"),
    };
    assert_eq!(next.payload, b"request!");
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id: next_id,
                result: serde_json::to_value(MiddlewareNextResponse {
                    metadata: None,
                    response: "response-1".into(),
                    protocol: "openai".into(),
                    status: 200,
                    headers: vec![MiddlewareHeader {
                        name: "x-upstream".into(),
                        value: b"old".to_vec(),
                    }],
                    body: Some(MiddlewareBodyHandle {
                        handle: "body-1".into(),
                        framing: MiddlewareBodyFraming::SseEvent,
                    }),
                })
                .unwrap(),
            },
            payload: Vec::new(),
        },
    )
    .await
    .unwrap();
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 1_024,
            frames: 2,
        },
    )
    .await;

    let initial = receive(&mut host).await;
    let Message::Result { id: 1, result } = initial.message else {
        panic!("expected middleware result")
    };
    let head: MiddlewareResponseHead = serde_json::from_value(result).unwrap();
    assert_eq!(head.header_mutations.len(), 2);
    assert!(matches!(
        &head.header_mutations[0],
        MiddlewareHeaderMutation::Remove { name } if name == "x-upstream"
    ));
    assert!(matches!(
        &head.header_mutations[1],
        MiddlewareHeaderMutation::Append { name, value }
            if name == "x-direct-response" && value == b"response"
    ));
    assert!(matches!(
        head.body,
        MiddlewareResponseBody::Stream {
            framing: MiddlewareBodyFraming::SseEvent
        }
    ));

    let read = receive(&mut host).await;
    let read_id = match read.message {
        Message::Callback {
            id,
            parent_id: 1,
            ref method,
            ..
        } if method == BODY_READ_METHOD => id,
        message => panic!("unexpected middleware body read: {message:?}"),
    };
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id: read_id,
                result: serde_json::to_value(MiddlewareBodyReadResult {
                    framing: MiddlewareBodyFraming::SseEvent,
                    source_id: 1,
                    eof: false,
                    terminal: true,
                })
                .unwrap(),
            },
            payload: b"data: done\n\n".to_vec(),
        },
    )
    .await
    .unwrap();
    let mapped = receive(&mut host).await;
    let Frame {
        message: Message::Stream { id: 1, sequence: 0 },
        payload,
    } = mapped
    else {
        panic!("expected mapped middleware frame")
    };
    let mapped = MiddlewareBodyFrame::decode(&payload).unwrap();
    assert_eq!(mapped.payload, b"DATA: DONE\n\n");
    assert!(!mapped.terminal);
    assert_eq!(mapped.source_id(), 1);
    assert_eq!(mapped.disposition(), MiddlewareBodyDisposition::Only);

    let eof = receive(&mut host).await;
    let eof_id = match eof.message {
        Message::Callback {
            id,
            parent_id: 1,
            ref method,
            ..
        } if method == BODY_READ_METHOD => id,
        message => panic!("unexpected middleware eof read: {message:?}"),
    };
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id: eof_id,
                result: serde_json::to_value(MiddlewareBodyReadResult {
                    framing: MiddlewareBodyFraming::SseEvent,
                    source_id: 0,
                    eof: true,
                    terminal: false,
                })
                .unwrap(),
            },
            payload: Vec::new(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn untouched_middleware_response_transfers_opaque_body_without_reading_it() {
    let (mut host, task) = start_session(middleware_plugin(false)).await;
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Call {
                id: 1,
                method: HANDLE_METHOD.into(),
                context: middleware_context(1),
                params: serde_json::to_value(middleware_request()).unwrap(),
            },
            payload: b"hidden-by-preserve".to_vec(),
        },
    )
    .await
    .unwrap();

    let next = receive(&mut host).await;
    let next_id = match next.message {
        Message::Callback {
            id,
            parent_id: 1,
            ref method,
            ref params,
        } if method == NEXT_METHOD => {
            let request: MiddlewareNextRequest = serde_json::from_value(params.clone()).unwrap();
            assert_eq!(request.body, MiddlewareRequestBody::Preserve);
            id
        }
        message => panic!("unexpected middleware next frame: {message:?}"),
    };
    assert!(next.payload.is_empty());
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id: next_id,
                result: serde_json::to_value(MiddlewareNextResponse {
                    metadata: None,
                    response: "response-1".into(),
                    protocol: "openai".into(),
                    status: 200,
                    headers: Vec::new(),
                    body: Some(MiddlewareBodyHandle {
                        handle: "body-pass-through".into(),
                        framing: MiddlewareBodyFraming::RawBytes,
                    }),
                })
                .unwrap(),
            },
            payload: Vec::new(),
        },
    )
    .await
    .unwrap();
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 1,
            frames: 1,
        },
    )
    .await;

    let initial = receive(&mut host).await;
    let Message::Result { id: 1, result } = initial.message else {
        panic!("expected middleware result")
    };
    let head: MiddlewareResponseHead = serde_json::from_value(result).unwrap();
    assert!(matches!(
        head.body,
        MiddlewareResponseBody::PassThrough {
            body: MiddlewareBodyHandle { ref handle, .. }
        } if handle == "body-pass-through"
    ));
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End { id: 1, error: None }
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn large_handler_results_succeed_and_oversized_faults_keep_the_session_available() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "large_result", json!({}), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, result }
            if result == json!("x".repeat(MAXIMUM_STREAM_CHUNK_BYTES))
    ));

    send_call(&mut host, 3, "oversized_fault", json!({}), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 3,
            error: PluginFault {
                code: ErrorCode::Fault,
                ref message,
                ..
            },
        } if message == "plugin error metadata exceeds its limit"
    ));

    send_call(
        &mut host,
        5,
        "dynamic_oversized_fault",
        json!({}),
        Vec::new(),
    )
    .await;
    send_control(
        &mut host,
        Message::Credit {
            id: 5,
            bytes: 1_024,
            frames: 1,
        },
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 5, .. }
    ));
    assert!(matches!(
        receive(&mut host).await.message,
        Message::End {
            id: 5,
            error: Some(PluginFault {
                code: ErrorCode::Fault,
                ref message,
                ..
            }),
        } if message == "plugin stream error metadata exceeds its limit"
    ));

    send_call(&mut host, 7, "echo", json!("still-ready"), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 7, result } if result == json!("still-ready")
    ));
    shutdown(&mut host, task).await;
}
