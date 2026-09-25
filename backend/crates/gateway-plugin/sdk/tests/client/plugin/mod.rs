use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use gateway_plugin_sdk::{
    CallContext, Contributions, ErrorCode, Frame, Handshake, Message, PROTOCOL_VERSION, Stage,
    call::{
        host::StateMigrationResult,
        management::{
            CommandDescriptor, CommandInvocation, CommandRegistration, CommandResult,
            ManagementRegistration, ManagementRequest, ManagementResponse, ManagementRoute,
        },
        registration::Registration,
    },
    client::{
        AuthorError, PluginBuilder, PluginHandler, PluginSession, SessionConfig, SessionError,
        TypedReply, methods, read_frame, write_frame,
    },
};
use serde_json::{Value, json};
use tokio::{
    io::{DuplexStream, ReadHalf, WriteHalf},
    task::JoinHandle,
};

const MAXIMUM_STREAM_CHUNK_BYTES: usize = 64 * 1024;

fn author_manifest(contributes: Value, state: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "manifestVersion": 1,
        "name": "composed",
        "displayName": "Composed",
        "publisher": "9acme",
        "version": "1.0.0",
        "description": "Composed plugin",
        "license": "MIT",
        "engines": {"codex-proxy-rs": "*"},
        "main": "bin/plugin",
        "runtime": "trustedProcess",
        "contributes": contributes,
        "state": state,
    }))
    .unwrap()
}

fn management_registration() -> ManagementRegistration {
    ManagementRegistration {
        routes: vec![ManagementRoute {
            method: "POST".into(),
            path: "echo".into(),
            request_content_types: vec!["application/octet-stream".into()],
            response_content_types: vec!["application/octet-stream".into()],
        }],
        resources: Vec::new(),
        pages: Vec::new(),
        callbacks: Vec::new(),
    }
}

fn command_registration() -> CommandRegistration {
    CommandRegistration {
        commands: vec![CommandDescriptor {
            name: "echo".into(),
            description: "Echo a value".into(),
            parameters: Vec::new(),
        }],
    }
}

#[tokio::test]
async fn composed_plugin_registers_and_dispatches_multiple_typed_entries() {
    let source = author_manifest(
        json!({
            "management": {},
            "command_line": {},
        }),
        json!([]),
    );
    let manifest = gateway_plugin_sdk::Manifest::from_author_slice(&source).unwrap();
    let contributions = manifest.contributes.clone();
    let management_calls = Arc::new(AtomicUsize::new(0));
    let management_calls_for_handler = Arc::clone(&management_calls);
    let plugin = PluginBuilder::from_manifest(manifest)
        .unwrap()
        .management(management_registration(), move |call| {
            let management_calls = Arc::clone(&management_calls_for_handler);
            async move {
                management_calls.fetch_add(1, Ordering::Relaxed);
                Ok(TypedReply::new(ManagementResponse {
                    status: 201,
                    content_type: call
                        .request
                        .content_type
                        .unwrap_or_else(|| "application/octet-stream".into()),
                })
                .with_payload(call.payload))
            }
        })
        .unwrap()
        .command_line(command_registration(), |call| async move {
            Ok(TypedReply::new(CommandResult {
                stdout: call.request.name,
                stderr: String::new(),
                exit_code: 0,
                accounts: Vec::new(),
            }))
        })
        .unwrap()
        .build()
        .unwrap();
    let (mut host, task) = start_session(plugin, contributions.clone()).await;

    send_call(
        &mut host,
        1,
        "plugin.register",
        Stage::Registration,
        json!({}),
        Vec::new(),
    )
    .await;
    let (result, payload) = unwrap_result(receive(&mut host).await, 1);
    let registration: Registration = serde_json::from_value(result).unwrap();
    assert_eq!(registration.contributes, contributions);
    assert!(payload.is_empty());

    send_call(
        &mut host,
        3,
        "management.register",
        Stage::Registration,
        json!({}),
        Vec::new(),
    )
    .await;
    let (result, payload) = unwrap_result(receive(&mut host).await, 3);
    assert_eq!(result, json!({}));
    let registration: ManagementRegistration = serde_json::from_slice(&payload).unwrap();
    assert_eq!(registration.routes.len(), 1);
    assert_eq!(registration.routes[0].path, "echo");

    send_call(
        &mut host,
        5,
        "management.handle",
        Stage::Management,
        serde_json::to_value(ManagementRequest {
            method: "POST".into(),
            path: "echo".into(),
            query: String::new(),
            content_type: Some("text/plain".into()),
        })
        .unwrap(),
        b"typed body".to_vec(),
    )
    .await;
    let (result, payload) = unwrap_result(receive(&mut host).await, 5);
    let response: ManagementResponse = serde_json::from_value(result).unwrap();
    assert_eq!(response.status, 201);
    assert_eq!(response.content_type, "text/plain");
    assert_eq!(payload, b"typed body");
    assert_eq!(management_calls.load(Ordering::Relaxed), 1);

    send_call(
        &mut host,
        7,
        "command_line.register",
        Stage::Registration,
        json!({}),
        Vec::new(),
    )
    .await;
    let (result, payload) = unwrap_result(receive(&mut host).await, 7);
    assert_eq!(result, json!({}));
    let registration: CommandRegistration = serde_json::from_slice(&payload).unwrap();
    assert_eq!(registration.commands.len(), 1);
    assert_eq!(registration.commands[0].name, "echo");

    let invocation = CommandInvocation {
        name: "echo".into(),
        arguments: BTreeMap::new(),
    };
    send_call(
        &mut host,
        9,
        "command_line.execute",
        Stage::CommandLine,
        json!({}),
        serde_json::to_vec(&invocation).unwrap(),
    )
    .await;
    let (result, payload) = unwrap_result(receive(&mut host).await, 9);
    assert_eq!(result, json!({}));
    let output: CommandResult = serde_json::from_slice(&payload).unwrap();
    assert_eq!(output.stdout, "echo");
    assert_eq!(output.exit_code, 0);

    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn catalog_and_retry_entries_use_typed_registration_and_stage_validation() {
    use gateway_plugin_sdk::call::{
        catalog::{ModelAlias, ModelCatalogRegistration},
        policy::{RetryAction, RetryDecision, RetryDecisionRequest},
    };
    let source = author_manifest(json!({"model_catalog":{},"retry_policy":{}}), json!([]));
    let manifest = gateway_plugin_sdk::Manifest::from_author_slice(&source).unwrap();
    let contributions = manifest.contributes.clone();
    let catalog = ModelCatalogRegistration {
        models: vec![ModelAlias {
            id: "public-model".into(),
            provider: "openai".into(),
            model: "upstream-model".into(),
        }],
    };
    let plugin = PluginBuilder::from_manifest(manifest)
        .unwrap()
        .model_catalog(catalog.clone())
        .unwrap()
        .on(methods::RETRY_DECISION, |call| async move {
            Ok(TypedReply::new(
                if call.request.allowed_actions.contains(&RetryAction::Retry) {
                    RetryDecision::Retry
                } else {
                    RetryDecision::Delegate
                },
            ))
        })
        .unwrap()
        .build()
        .unwrap();
    let (mut host, task) = start_session(plugin, contributions).await;
    send_call(
        &mut host,
        1,
        "model_catalog.register",
        Stage::Registration,
        json!({}),
        vec![],
    )
    .await;
    let (result, payload) = unwrap_result(receive(&mut host).await, 1);
    assert_eq!(result, json!({}));
    assert_eq!(
        serde_json::from_slice::<ModelCatalogRegistration>(&payload).unwrap(),
        catalog
    );
    let request = serde_json::to_value(RetryDecisionRequest {
        request_id: "req-example".into(),
        attempt_index: 1,
        provider: "openai".into(),
        model: Some("upstream-model".into()),
        error_kind: "rate_limited".into(),
        upstream_status: Some(429),
        send_state: gateway_plugin_sdk::SendState::NotSent,
        remaining_routing_attempts: 1,
        remaining_deadline_ms: 1_000,
        allowed_actions: vec![RetryAction::Stop, RetryAction::Retry],
    })
    .unwrap();
    send_call(
        &mut host,
        3,
        "policy.retry_decision",
        Stage::Request,
        request.clone(),
        vec![],
    )
    .await;
    assert_invalid_input(receive(&mut host).await, 3);
    send_call(
        &mut host,
        5,
        "policy.retry_decision",
        Stage::Retry,
        request,
        vec![],
    )
    .await;
    let (result, payload) = unwrap_result(receive(&mut host).await, 5);
    assert_eq!(result, json!({"decision":"retry"}));
    assert!(payload.is_empty());
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn typed_dispatch_rejects_wrong_stage_and_payload_shape_before_handler() {
    let source = author_manifest(json!({"command_line": {}}), json!([]));
    let manifest = gateway_plugin_sdk::Manifest::from_author_slice(&source).unwrap();
    let contributions = manifest.contributes.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&calls);
    let plugin = PluginBuilder::from_manifest(manifest)
        .unwrap()
        .command_line(command_registration(), move |_| {
            let calls = Arc::clone(&calls_for_handler);
            async move {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(TypedReply::new(CommandResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 0,
                    accounts: Vec::new(),
                }))
            }
        })
        .unwrap()
        .build()
        .unwrap();
    let (mut host, task) = start_session(plugin, contributions).await;
    let invocation = serde_json::to_vec(&CommandInvocation {
        name: "echo".into(),
        arguments: BTreeMap::new(),
    })
    .unwrap();

    send_call(
        &mut host,
        1,
        "command_line.execute",
        Stage::Management,
        json!({}),
        invocation.clone(),
    )
    .await;
    assert_invalid_input(receive(&mut host).await, 1);

    send_call(
        &mut host,
        3,
        "command_line.execute",
        Stage::CommandLine,
        json!({"name": "must-not-be-control-metadata"}),
        invocation,
    )
    .await;
    assert_invalid_input(receive(&mut host).await, 3);

    send_call(
        &mut host,
        5,
        "command_line.execute",
        Stage::CommandLine,
        json!({}),
        b"not json".to_vec(),
    )
    .await;
    assert_invalid_input(receive(&mut host).await, 5);
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    shutdown(&mut host, task).await;
}

#[test]
fn build_reports_missing_declared_handler() {
    let source = author_manifest(json!({"management": {}}), json!([]));
    let result = PluginBuilder::from_json(&source).unwrap().build();
    assert!(matches!(
        result,
        Err(AuthorError::MissingMethod("management.register"))
    ));
}

#[test]
fn state_migration_is_a_base_method_gated_by_the_state_declaration() {
    let state = json!([{
        "namespace": "documents",
        "schemaVersion": 2,
        "schema": {"type": "object"},
        "maximumRecords": 16,
        "maximumBytes": 4096,
        "maximumValueBytes": 1024,
        "migratesFrom": [1]
    }]);
    let source = author_manifest(json!({}), state);
    let missing = PluginBuilder::from_json(&source).unwrap().build();
    assert!(matches!(
        missing,
        Err(AuthorError::MissingMethod("plugin.state.migrate"))
    ));

    let plugin = PluginBuilder::from_json(&source)
        .unwrap()
        .on(methods::STATE_MIGRATE, |_| async {
            Ok(TypedReply::new(StateMigrationResult {
                changes: Vec::new(),
            }))
        })
        .unwrap()
        .build();
    assert!(plugin.is_ok());

    let source_without_migration = author_manifest(json!({}), json!([]));
    let undeclared = PluginBuilder::from_json(&source_without_migration)
        .unwrap()
        .on(methods::STATE_MIGRATE, |_| async {
            Ok(TypedReply::new(StateMigrationResult {
                changes: Vec::new(),
            }))
        });
    assert!(matches!(
        undeclared,
        Err(AuthorError::Method("plugin.state.migrate"))
    ));
}

struct HostPeer {
    reader: ReadHalf<DuplexStream>,
    writer: WriteHalf<DuplexStream>,
}

async fn start_session<H: PluginHandler + 'static>(
    handler: H,
    contributes: Contributions,
) -> (HostPeer, JoinHandle<Result<(), SessionError>>) {
    let (host, plugin) = tokio::io::duplex(MAXIMUM_STREAM_CHUNK_BYTES * 2);
    let (plugin_reader, plugin_writer) = tokio::io::split(plugin);
    let task = tokio::spawn(async move {
        PluginSession::accept(
            plugin_reader,
            plugin_writer,
            SessionConfig {
                maximum_stream_chunk_bytes: MAXIMUM_STREAM_CHUNK_BYTES,
                maximum_calls: 4,
                maximum_callbacks: 4,
                maximum_buffered_stream_chunks: 16,
                handshake_timeout: Duration::from_secs(1),
                maximum_call_timeout: Duration::from_secs(2),
            },
        )
        .await?
        .run(handler)
        .await
    });
    let (mut reader, mut writer) = tokio::io::split(host);
    write_frame(
        &mut writer,
        &Frame::control(Message::Hello {
            handshake: Handshake {
                protocol_version: PROTOCOL_VERSION,
                artifact_sha256: "a".repeat(64),
                plugin_id: "9acme.composed".into(),
                instance_id: "test-instance".into(),
                generation: 1,
                incarnation: "test-incarnation".into(),
                configuration: json!({}),
                permissions: Vec::new(),
                contributes,
            },
        }),
    )
    .await
    .unwrap();
    let ready = read_frame(&mut reader).await.unwrap();
    assert!(matches!(
        ready,
        Frame {
            message: Message::Ready {
                protocol_version: PROTOCOL_VERSION,
                ref incarnation,
            },
            ref payload,
        } if incarnation == "test-incarnation" && payload.is_empty()
    ));
    (HostPeer { reader, writer }, task)
}

async fn send_call(
    host: &mut HostPeer,
    id: u64,
    method: &str,
    stage: Stage,
    params: Value,
    payload: Vec<u8>,
) {
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Call {
                id,
                method: method.into(),
                context: CallContext {
                    call_id: id,
                    instance_id: "test-instance".into(),
                    generation: 1,
                    incarnation: "test-incarnation".into(),
                    stage,
                    timeout_ms: 1_000,
                    resource_scope_id: format!("scope-{id}"),
                    request_id: None,
                    attempt_id: None,
                    account_id: None,
                    credential_revision: None,
                },
                params,
            },
            payload,
        },
    )
    .await
    .unwrap();
}

async fn receive(host: &mut HostPeer) -> Frame {
    tokio::time::timeout(Duration::from_secs(1), read_frame(&mut host.reader))
        .await
        .expect("plugin response timed out")
        .expect("plugin response frame must be valid")
}

fn unwrap_result(frame: Frame, expected_id: u64) -> (Value, Vec<u8>) {
    match frame {
        Frame {
            message: Message::Result { id, result },
            payload,
        } if id == expected_id => (result, payload),
        unexpected => panic!("unexpected plugin response: {unexpected:?}"),
    }
}

fn assert_invalid_input(frame: Frame, expected_id: u64) {
    assert!(matches!(
        frame,
        Frame {
            message: Message::Error { id, error },
            ref payload,
        } if id == expected_id && error.code == ErrorCode::InvalidInput && payload.is_empty()
    ));
}

async fn shutdown(host: &mut HostPeer, task: JoinHandle<Result<(), SessionError>>) {
    write_frame(&mut host.writer, &Frame::control(Message::Shutdown))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("plugin session did not shut down")
        .expect("plugin session task panicked")
        .expect("plugin session shutdown failed");
}
