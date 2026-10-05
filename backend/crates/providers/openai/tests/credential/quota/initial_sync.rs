//! 首查随机启动延迟与失败重排的确定性边界（通过注入延迟采样器驱动）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use provider_openai::credential::CodexInitialSyncDelays;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

fn usage_ok() -> serde_json::Value {
    serde_json::json!({
        "rate_limit": {"allowed": true, "primary_window": {"used_percent": 10}}
    })
}

fn zero_delays() -> CodexInitialSyncDelays {
    Arc::new(|| Duration::ZERO)
}

fn fixed_delays(delay: Duration) -> CodexInitialSyncDelays {
    Arc::new(move || delay)
}

/// 按给定顺序逐个返回脚本化延迟，耗尽后退化为零延迟。
fn scripted_delays(delays: Vec<Duration>) -> CodexInitialSyncDelays {
    let queue = Arc::new(Mutex::new(VecDeque::from(delays)));
    Arc::new(move || {
        queue
            .lock()
            .expect("delay queue")
            .pop_front()
            .unwrap_or(Duration::ZERO)
    })
}

async fn mount_usage(server: &MockServer, status: u16) {
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(status).set_body_json(usage_ok()))
        .mount(server)
        .await;
}

async fn usage_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|request| request.url.path() == "/api/codex/usage")
        .count()
}

fn local_service(
    store: &Arc<MemoryAccountStore>,
    server: &MockServer,
    delays: CodexInitialSyncDelays,
) -> CodexCredentialQuotaService {
    quota_service_with_initial_delays(
        store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
        delays,
    )
}

#[tokio::test]
async fn zero_initial_delay_keeps_immediate_first_observation() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_initial_zero").await;
    let server = MockServer::start().await;
    mount_usage(&server, 200).await;
    let service = local_service(&store, &server, zero_delays());

    assert_eq!(
        service.synchronize().await.expect("first cycle").updated,
        1,
        "零延迟必须保持首轮即查的旧行为"
    );
    // 已产生快照后退出首查路径，正常账号也不满足周期复核条件。
    service.synchronize().await.expect("second cycle");
    assert_eq!(usage_requests(&server).await, 1);
}

#[tokio::test]
async fn deferred_initial_delay_holds_back_first_observation() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_initial_deferred").await;
    let server = MockServer::start().await;
    mount_usage(&server, 200).await;
    let service = local_service(&store, &server, fixed_delays(Duration::from_secs(3600)));

    for cycle in 0..2 {
        assert_eq!(
            service
                .synchronize()
                .await
                .unwrap_or_else(|_| panic!("deferred cycle {cycle} failed"))
                .updated,
            0,
            "随机延迟未到期前不得发起首查"
        );
    }
    assert_eq!(usage_requests(&server).await, 0);
}

#[tokio::test]
async fn per_account_delays_are_drawn_independently() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_initial_first").await;
    create_account(&store, "acct_initial_second").await;
    let server = MockServer::start().await;
    mount_usage(&server, 200).await;
    // 首个候选账号零延迟立即观察，次个账号延迟 1 小时。
    let service = local_service(
        &store,
        &server,
        scripted_delays(vec![Duration::ZERO, Duration::from_secs(3600)]),
    );

    assert_eq!(
        service.synchronize().await.expect("first cycle").updated,
        1,
        "只有延迟到期的账号参与本轮首查"
    );
    service.synchronize().await.expect("second cycle");
    assert_eq!(usage_requests(&server).await, 1);
}

#[tokio::test]
async fn initial_sync_batch_cap_bounds_first_round() {
    let store = Arc::new(MemoryAccountStore::default());
    for index in 0..150 {
        create_account(&store, &format!("acct_initial_cap_{index:03}")).await;
    }
    let server = MockServer::start().await;
    mount_usage(&server, 200).await;
    let service = local_service(&store, &server, zero_delays());

    assert_eq!(
        service.synchronize().await.expect("first round").updated,
        100,
        "单轮首查仍受批量上限约束"
    );
    assert_eq!(
        service.synchronize().await.expect("second round").updated,
        50,
        "剩余候选在后续轮次补齐"
    );
    assert_eq!(usage_requests(&server).await, 150);
}

#[tokio::test]
async fn failed_initial_attempt_is_rearmed_with_fresh_delay() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_initial_retry").await;
    let server = MockServer::start().await;
    // 429 属非 5xx 的瞬时上游拒绝，不触发 usage 拉取自身的退避重试。
    mount_usage(&server, 429).await;
    // 首查分配零延迟（立即观察），失败后重排 1 小时。
    let service = local_service(
        &store,
        &server,
        scripted_delays(vec![Duration::ZERO, Duration::from_secs(3600)]),
    );

    assert_eq!(
        service
            .synchronize()
            .await
            .expect("failing first attempt")
            .transient,
        1
    );
    assert_eq!(usage_requests(&server).await, 1);
    service.synchronize().await.expect("rearmed cycle");
    assert_eq!(
        usage_requests(&server).await,
        1,
        "失败重排的随机延迟未到期，不得回到固定 30s 节奏重复请求"
    );
}
