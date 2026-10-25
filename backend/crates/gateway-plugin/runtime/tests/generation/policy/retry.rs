//! 验证真实重试插件的 HTTP 与服务回调副作用会阻止 Core 透明重放

use super::*;
use gateway_core::{
    engine::{
        AttemptContext,
        execution::{ExecutionRequestMetadata, StartExecution},
        provider::{
            Provider, ProviderCallMetadata, ProviderRegistry, ProviderRequest, ProviderStream,
        },
    },
    error::{ProviderError, ProviderErrorKind},
    routing::{
        ModelCapabilities, ProviderCatalogGeneration, ProviderModelCapabilities, UpstreamModelId,
    },
    upstream::{UpstreamSendState, UpstreamTransport},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct RetryableProvider {
    account: ProviderAccountId,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Provider for RetryableProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(vec![ProviderModelCapabilities::new(
            UpstreamModelId::new("retry-fixture").unwrap(),
            ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None),
        )])
    }

    async fn execute(
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) != 0 {
            return Err(ProviderError::new(
                ProviderErrorKind::NoEligibleAccount,
                UpstreamSendState::NotSent,
            ));
        }
        Ok(ProviderStream::new(
            ProviderCallMetadata::new(
                request.candidate().provider().clone(),
                request.candidate().upstream_model().cloned().unwrap(),
                self.account.clone(),
                UpstreamTransport::new("http").unwrap(),
            ),
            futures::stream::iter([Err(ProviderError::new(
                ProviderErrorKind::Transport,
                UpstreamSendState::NotSent,
            )
            .with_replay_safe())]),
            (),
        ))
    }
}

#[tokio::test]
async fn retry_callback_effects_survive_success_failure_and_cancellation() {
    for outcome in [
        "none",
        "not_sent",
        "sent",
        "ambiguous",
        "fault",
        "cancelled",
        "early_result",
        "service",
        "service_error",
        "service_missing",
        "service_invalid",
        "service_unbound",
    ] {
        let Some(environment) = Environment::create().await else {
            return;
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = if outcome == "not_sent" {
            "not-an-http-url".to_owned()
        } else {
            format!("http://{}/effect", listener.local_addr().unwrap())
        };
        let accepted = Arc::new(AtomicUsize::new(0));
        let requests = accepted.clone();
        let marker = environment.directory.path().join("http-received");
        let received = marker.clone();
        let service_marker = environment.directory.path().join("service-result.jsonl");
        let is_service = outcome.starts_with("service");
        let is_http = outcome != "none" && !is_service;
        let service_request = match outcome {
            "service_invalid" => serde_json::json!({"operation":123}),
            "service_missing" => serde_json::json!({"operation":"settings.missing", "input":null}),
            "service_error" => {
                serde_json::json!({"operation":"settings.preview_client_profile", "input":["missing-provider", null]})
            }
            _ => serde_json::json!({"operation":"settings.load", "input":null}),
        };
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 1024];
            assert_ne!(
                stream.read(&mut buffer).await.unwrap(),
                0,
                "HTTP request bytes"
            );
            requests.fetch_add(1, Ordering::SeqCst);
            std::fs::write(received, []).unwrap();
            if matches!(outcome, "cancelled" | "early_result") {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            if outcome != "ambiguous" {
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                    .await;
            }
        });
        environment
            .install_plugin(serde_json::json!({
                "retry_decision":{"decision":"retry"},
                "retry_http_url":is_http.then_some(url),
                "retry_fault":outcome == "fault",
                "retry_http_return_marker":(outcome == "early_result").then_some(marker),
                "retry_service_request":is_service.then_some(service_request),
                "retry_service_marker":service_marker,
                "call_budgets_ms":{"retry":500},
            }))
            .await;
        let key = "sk-retry-effects-fixture";
        environment.client_key("key_retry_effects", key).await;
        let provider = Arc::new(RetryableProvider {
            account: environment.account(None).await,
            calls: AtomicUsize::new(0),
        });
        let (runtime, core) = environment
            .runtime_with_providers(
                RpcLimits::default(),
                ProviderRegistry::new([provider.clone() as Arc<dyn Provider>]).unwrap(),
            )
            .await;
        let admin = if is_service && outcome != "service_unbound" {
            let admin = environment.bind_admin_accounts(&runtime, &core).await;
            runtime
                .bind_services(&admin.services().public_services())
                .unwrap();
            Some(admin)
        } else {
            None
        };
        let execution = core.execution_service();
        let mut started = execution
            .start(StartExecution {
                client: execution.authenticate(key).unwrap(),
                public_model: PublicModelId::new("retry-fixture").unwrap(),
                operation: operation(),
                metadata: ExecutionRequestMetadata {
                    protocol: "openai".into(),
                    endpoint: "/v1/responses".into(),
                    transport: ClientTransport::HttpJson,
                    stream: false,
                    client_ip: None,
                    user_agent: None,
                    previous_response_id: None,
                },
            })
            .await
            .unwrap();
        assert!(started.session.collect_uncommitted().await.is_err());
        started.session.detach_finalize().await;
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            if matches!(
                outcome,
                "none" | "not_sent" | "service_missing" | "service_invalid" | "service_unbound"
            ) {
                2
            } else {
                1
            },
            "{outcome}"
        );
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            usize::from(is_http && outcome != "not_sent"),
            "{outcome}"
        );
        if is_service {
            let record: serde_json::Value = serde_json::from_str(
                std::fs::read_to_string(service_marker)
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap(),
            )
            .unwrap();
            match outcome {
                "service" => assert!(record.get("Ok").is_some(), "{record}"),
                "service_error" | "service_missing" => {
                    assert!(record.get("Err").is_some(), "{record}")
                }
                _ => assert!(record.get("callback_error").is_some(), "{record}"),
            }
        }
        server.abort();
        let _ = server.await;
        if admin.is_none() {
            environment.release_plugin_accounts(&runtime);
        }
        drop(admin);
        drop(core);
        runtime.shutdown().await;
        environment.close().await;
    }
}
