//! 验证 OpenAI 目录 ETag worker 的失败交接、重启恢复与取消

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use gateway_core::lifecycle::CancellationToken;
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload};
use gateway_core::task::{WorkerContribution, WorkerDaemonContext, WorkerRunnable};
use provider_openai::credential::ImportCodexOAuthCredential;
use serde_json::{Map, json};
use tokio::time::timeout;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::admin::{
    initialized_attempt_context, initialized_provider_request, initialized_test_bundle,
};
use crate::support::{MemoryAccountStore, profile, secret};

#[derive(Default)]
struct ReleaseProxyPolicy(std::sync::Mutex<Option<gateway_core::account::OutboundProxy>>);

impl gateway_core::provider_ports::ProviderRuntimePolicyPort for ReleaseProxyPolicy {
    fn load_client_release_proxy(
        &self,
    ) -> futures::future::BoxFuture<
        '_,
        Result<
            Option<gateway_core::account::OutboundProxy>,
            gateway_core::provider_ports::ProviderStoreError,
        >,
    > {
        Box::pin(async { Ok(self.0.lock().unwrap().clone()) })
    }

    fn load_refresh_policy(
        &self,
    ) -> futures::future::BoxFuture<
        '_,
        Result<
            gateway_core::provider_ports::ProviderRefreshPolicy,
            gateway_core::provider_ports::ProviderStoreError,
        >,
    > {
        Box::pin(async {
            gateway_core::provider_ports::ProviderRefreshPolicy::try_new(
                Duration::from_secs(300),
                std::num::NonZeroU32::new(4).unwrap(),
            )
        })
    }
}

#[tokio::test]
async fn release_worker_rechecks_after_proxy_change_without_restarting() {
    use gateway_core::account::OutboundProxy;
    use gateway_core::task::WorkerCycleContext;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::net::TcpListener;

    let policy = Arc::new(ReleaseProxyPolicy::default());
    let mut bundle = crate::admin::initialized_test_bundle_with_policy(
        Arc::new(MemoryAccountStore::default()),
        "http://127.0.0.1:1".into(),
        policy.clone(),
    )
    .await;
    let registration = bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|contribution| match contribution {
            WorkerContribution::Registration(registration)
                if registration.id.owner() == "openai-cli-release" =>
            {
                Some(registration)
            }
            _ => None,
        })
        .unwrap();
    let WorkerRunnable::Scheduled { task, .. } = registration.runnable else {
        panic!("scheduled release task")
    };
    let context = WorkerCycleContext::new(registration.id, None, CancellationToken::new());
    for _ in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        *policy.0.lock().unwrap() = Some(
            OutboundProxy::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap(),
        );
        let proxy = async {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line, "CONNECT registry.npmjs.org:443 HTTP/1.1\r\n");
            reader
                .get_mut()
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        };
        let (result, ()) = timeout(Duration::from_secs(3), async {
            tokio::join!(task.run_cycle(context.clone()), proxy)
        })
        .await
        .unwrap();
        result.unwrap();
        // 未改出口时只读配置，不在每个调度周期重复访问官方源
        timeout(Duration::from_millis(100), task.run_cycle(context.clone()))
            .await
            .unwrap()
            .unwrap();
        assert!(
            timeout(Duration::from_millis(25), listener.accept())
                .await
                .is_err()
        );
    }
    *policy.0.lock().unwrap() = None;
    context.cancellation().cancel();
    timeout(Duration::from_millis(100), task.run_cycle(context))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn catalog_etag_failure_returns_to_supervisor_and_recovers_pending_refresh() {
    let account_id = "acct_catalog_etag_worker";
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_owned(),
            name: account_id.to_owned(),
            secret: secret("at-catalog-etag-worker"),
            verified_account: profile("chatgpt-catalog-etag-worker"),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-models-etag", "models-worker-v2")
                .set_body_string(concat!(
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_etag\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[]}}\n\n"
                )),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            include_bytes!("../transport/fixtures/official_models_snapshot.json"),
            "application/json",
        ))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let mut bundle = initialized_test_bundle(store, server.uri()).await;
    let provider = bundle.core_provider();
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model": "gpt-5.4", "input": "catalog refresh"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap()
        .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
    ));
    let mut stream = provider
        .execute(
            initialized_provider_request(operation, account_id),
            initialized_attempt_context("req_catalog_etag_worker", account_id),
        )
        .await
        .expect("provider stream");
    while let Some(event) = stream.next().await {
        event.expect("response carrying catalog ETag");
    }
    let worker = bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|contribution| match contribution {
            WorkerContribution::Registration(registration)
                if registration.id.owner() == "openai-model-etag" =>
            {
                Some(registration)
            }
            _ => None,
        })
        .expect("catalog ETag worker");
    let WorkerRunnable::Daemon { task, .. } = worker.runnable else {
        panic!("catalog ETag worker must be a daemon");
    };

    for expected_requests in 1..=2 {
        let context = WorkerDaemonContext::new(CancellationToken::new());
        let result = timeout(Duration::from_secs(2), task.run(context.clone())).await;
        let requests = server.received_requests().await.unwrap();
        let paths: Vec<_> = requests.iter().map(|request| request.url.path()).collect();
        assert!(
            result.is_ok(),
            "failed refresh must return control to the supervisor: {paths:?}, completed={}",
            context.completed_cycles()
        );
        let error = result.unwrap().expect_err("catalog unavailable");
        assert!(
            error.error_details().is_some(),
            "preserve the failure cause"
        );
        assert_eq!(context.completed_cycles(), 0);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.path() == "/codex/models")
                .count(),
            expected_requests,
            "each run must perform only one failed refresh"
        );
    }

    let context = WorkerDaemonContext::new(CancellationToken::new());
    let run = task.run(context.clone());
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => panic!("daemon exited before recovery: {result:?}"),
        result = timeout(Duration::from_secs(2), context.progress_since(0)) => {
            result.expect("pending ETag must survive failed runs");
        }
    }
    assert_eq!(context.completed_cycles(), 1);
    context.cancellation().cancel();
    timeout(Duration::from_secs(2), run)
        .await
        .expect("idle worker must stop on cancellation")
        .expect("cancelled worker");
}
