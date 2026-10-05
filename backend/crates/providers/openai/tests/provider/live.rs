//! Live 引导模型事实与固定账号重连授权回归

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bytes::Bytes;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::account::{
    AccountModelAccess, AccountModelAccessMode, ProviderAccountId, ProviderAccountStore,
};
use gateway_core::engine::middleware::{
    MiddlewareContext, MiddlewareError, MiddlewareNext, MiddlewarePlan, MiddlewareRequest,
    MiddlewareResponse,
};
use gateway_core::engine::provider::{Provider, ProviderRequest};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::live::{LiveGatewayErrorKind, LiveSidebandRequest, LiveSidebandStyle};
use gateway_core::operation::{Operation, ProviderHttpMethod, ProviderHttpRequest, RawHttpPayload};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::{
    ClientRoutingScope, ConfigRevision, FrozenAccountScope, ProviderKind, RoutingContext,
    RuntimeAccount, RuntimeAccountDirectory, RuntimeSnapshot, UpstreamModelId,
};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use super::contract::{context, context_with_middleware, create_account, provider_with_base_url};
use crate::support::MemoryAccountStore;

const MODEL: &str = "gpt-live-1-codex";
const ACCOUNT: &str = "acct_provider_contract";
const CALL: &str = "call_live_regression";

fn scope(model_access: AccountModelAccess) -> Arc<FrozenAccountScope> {
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new(ACCOUNT).unwrap(),
            RuntimeAccount::new(ProviderKind::new("openai").unwrap(), BTreeSet::new())
                .with_model_access(model_access),
        )]))),
        ClientRoutingScope::all_accounts(),
    ))
}

fn live_request() -> ProviderRequest {
    let provider = ProviderKind::new("openai").unwrap();
    let model = UpstreamModelId::new(MODEL).unwrap();
    let operation = Operation::ProviderHttp(
        ProviderHttpRequest::new(
            "realtime-calls",
            ProviderHttpMethod::Post,
            Some("intent=quicksilver&architecture=avas".into()),
            Vec::new(),
            RawHttpPayload::new(
                "openai",
                Bytes::from(json!({"sdp":"v=0", "session":{"model":MODEL}}).to_string()),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(2, 10, "smart", BTreeMap::new(), None, None),
        vec![provider.clone()],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let plan = snapshot
        .plan_provider_endpoint(
            &provider,
            Some(&model),
            &operation,
            scope(AccountModelAccess::default()),
            &RoutingContext::default(),
        )
        .unwrap();
    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

#[derive(Debug)]
struct ModelMiddleware;
impl MiddlewarePlan for ModelMiddleware {
    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        assert_eq!(context.model(), Some(MODEL));
        next.run(request)
    }
}

async fn bootstrap() -> (Arc<dyn Provider>, Arc<MemoryAccountStore>, MockServer) {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/realtime/calls"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("location", format!("/v1/live/{CALL}"))
                .insert_header("content-type", "application/sdp")
                .set_body_string("v=0\r\n"),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let provider = crate::admin::initialized_test_provider(store.clone(), upstream.uri()).await;
    let request = live_request();
    let candidate = request.candidate().clone();
    let mut stream = provider
        .clone()
        .execute(
            request,
            context_with_middleware("req_live_regression", Arc::new(ModelMiddleware), false),
        )
        .await
        .expect("live stream");
    assert!(
        stream.metadata().confirms(&candidate),
        "Core must accept the frozen provider/model facts"
    );
    assert!(
        upstream.received_requests().await.unwrap().is_empty(),
        "bootstrap must remain cold before Core accepts metadata"
    );
    while let Some(event) = stream.next().await {
        event.expect("bootstrap response");
    }
    (provider, store, upstream)
}

#[tokio::test]
async fn live_bootstrap_preserves_model_through_middleware_and_metadata() {
    let (_provider, _store, upstream) = bootstrap().await;
    upstream.verify().await;
}

#[tokio::test]
async fn live_sideband_rejects_revoked_scope_and_model_without_claiming_call() {
    let (provider, _store, _upstream) = bootstrap().await;
    let gateway = provider.live_gateway().unwrap();
    let denied_model = scope(
        AccountModelAccess::new(AccountModelAccessMode::Denylist, vec![MODEL.to_owned()]).unwrap(),
    );
    let absent_account = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::new())),
        ClientRoutingScope::all_accounts(),
    ));
    let key = ClientApiKeyId::new("key_openai_contract").unwrap();
    // 同一 call 连续拒绝不能遗留 claim，后续请求仍应得到权限错误而不是 busy
    for account_scope in [&absent_account, &denied_model, &absent_account] {
        let error = gateway
            .open_sideband(LiveSidebandRequest {
                call_id: CALL,
                client_api_key_id: &key,
                account_scope,
                style: LiveSidebandStyle::Live,
                protocol_headers: Vec::new(),
                subprotocols: Vec::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind(), LiveGatewayErrorKind::OwnerMismatch);
        assert_eq!(error.status(), 403);
    }
}

#[tokio::test]
async fn live_sideband_rejects_disabled_account_even_before_scope_refresh() {
    let (provider, store, _upstream) = bootstrap().await;
    store
        .set_enabled(&ProviderAccountId::new(ACCOUNT).unwrap(), false)
        .await
        .unwrap();
    let gateway = provider.live_gateway().unwrap();
    let key = ClientApiKeyId::new("key_openai_contract").unwrap();
    let allowed = scope(AccountModelAccess::default());
    for _ in 0..2 {
        let error = gateway
            .open_sideband(LiveSidebandRequest {
                call_id: CALL,
                client_api_key_id: &key,
                account_scope: &allowed,
                style: LiveSidebandStyle::Live,
                protocol_headers: Vec::new(),
                subprotocols: Vec::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.status(), 403);
    }
}

#[tokio::test]
async fn live_bootstrap_rejects_missing_planned_model_before_upstream_send() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let upstream = MockServer::start().await;
    let provider = provider_with_base_url(&store, upstream.uri());
    let request = live_request();
    let provider_kind = ProviderKind::new("openai").unwrap();
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(2, 10, "smart", BTreeMap::new(), None, None),
        vec![provider_kind.clone()],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let plan = snapshot
        .plan_provider_endpoint(
            &provider_kind,
            None,
            request.operation(),
            scope(AccountModelAccess::default()),
            &RoutingContext::default(),
        )
        .unwrap();
    let result = provider
        .execute(
            ProviderRequest::new(request.operation().clone(), plan.candidates()[0].clone()),
            context("req_live_missing_model", CancellationToken::new()),
        )
        .await;
    assert!(result.is_err());
    assert!(upstream.received_requests().await.unwrap().is_empty());
}
