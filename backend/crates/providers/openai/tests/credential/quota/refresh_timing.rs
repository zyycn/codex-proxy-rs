//! 通过 quota 服务验证耗尽复核节奏、恢复与正常窗口宽限期

use std::time::Duration;

use super::*;

async fn mount_usage(server: &MockServer, value: serde_json::Value) {
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(value))
        .mount(server)
        .await;
}

fn reset_grace(reset: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs((reset + 120) as u64)
}

async fn advance_clock(now: &mut SystemTime, seconds: u64) {
    let duration = Duration::from_secs(seconds);
    // 只在推进时暂停 Tokio 时钟，HTTP 请求期间保持真实计时
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
    *now += duration;
}

#[tokio::test]
async fn exhausted_reset_bypasses_periodic_throttle_and_rechecks_each_pending_window() {
    let mut now = SystemTime::now();
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_quota_timing").await;
    let account = store.account("acct_quota_timing").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    // 显式推进调度时刻，真实 HTTP 与落库路径仍完整执行
    let short_reset = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 5;
    let week_reset = short_reset + 30;
    let usage = |short_used, short_reset, week_used, week_reset| {
        json!({"rate_limit": {
            "allowed": false,
            "primary_window": {
                "used_percent": short_used, "reset_at": short_reset, "limit_window_seconds": 18_000,
            },
            "secondary_window": {
                "used_percent": week_used, "reset_at": week_reset, "limit_window_seconds": 604_800,
            },
        }})
    };
    mount_usage(&server, usage(100, short_reset, 100, week_reset)).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed exhaustion");
    service
        .synchronize_at(now)
        .await
        .expect("initial periodic check");
    let requests = server.received_requests().await.expect("requests").len();

    assert!(
        now < SystemTime::UNIX_EPOCH + Duration::from_secs(short_reset as u64),
        "fixture must precede reset"
    );
    service.synchronize_at(now).await.expect("before reset");
    assert_eq!(
        server.received_requests().await.expect("requests").len(),
        requests
    );

    mount_usage(&server, usage(0, short_reset + 18_000, 100, week_reset)).await;
    for delay in [0, 9] {
        now = SystemTime::UNIX_EPOCH + Duration::from_secs(short_reset as u64 + delay);
        service.synchronize_at(now).await.expect("reset grace");
        assert!(
            server
                .received_requests()
                .await
                .expect("requests")
                .is_empty()
        );
    }
    now = SystemTime::UNIX_EPOCH + Duration::from_secs(short_reset as u64 + 10);
    let partial = service
        .synchronize_at(now)
        .await
        .expect("short reset check");
    assert_eq!(partial.exhausted, 1);
    assert_eq!(
        store
            .account("acct_quota_timing")
            .expect("account")
            .quota()
            .reset_at(),
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(week_reset as u64))
    );
    service
        .synchronize_at(now)
        .await
        .expect("next scan before weekly reset");
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);

    // 周窗口的到期复核仍未恢复时，也不能每轮扫描重复请求
    now = SystemTime::UNIX_EPOCH + Duration::from_secs(week_reset as u64 + 10);
    assert_eq!(
        service
            .synchronize_at(now)
            .await
            .expect("weekly reset check")
            .exhausted,
        1
    );
    service
        .synchronize_at(now)
        .await
        .expect("repeat scan after weekly check");
    assert_eq!(server.received_requests().await.expect("requests").len(), 2);
}

#[tokio::test]
async fn periodic_checks_continue_when_reset_is_unknown_or_far_in_the_future() {
    for reset in [None, Some(Utc::now().timestamp() + 604_800)] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_quota_periodic").await;
        let account = store.account("acct_quota_periodic").expect("account");
        let server = MockServer::start().await;
        let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
        let mut usage = json!({"rate_limit": {
            "allowed": false,
            "primary_window": {"used_percent": 100, "limit_window_seconds": 604_800},
        }});
        if let Some(reset) = reset {
            usage["rate_limit"]["primary_window"]["reset_at"] = json!(reset);
        }
        mount_usage(&server, usage.clone()).await;
        service
            .refresh_account(account.id())
            .await
            .expect("seed exhaustion");
        mount_usage(&server, usage).await;

        let mut now = SystemTime::now();
        assert_eq!(
            service
                .synchronize_at(now)
                .await
                .expect("periodic check")
                .exhausted,
            1
        );
        advance_clock(&mut now, 29 * 60).await;
        service
            .synchronize_at(now)
            .await
            .expect("throttled repeat check");
        assert_eq!(server.received_requests().await.expect("requests").len(), 1);
        advance_clock(&mut now, 60).await;
        service.synchronize_at(now).await.expect("periodic retry");
        assert_eq!(server.received_requests().await.expect("requests").len(), 2);
    }
}

#[tokio::test]
async fn exhausted_reset_refreshes_quickly_then_backs_off_and_recovers_from_usage() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_reset_retry").await;
    let account = store.account("acct_reset_retry").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let reset = Utc::now().timestamp();
    let mut now = SystemTime::UNIX_EPOCH + Duration::from_secs(reset as u64);
    let usage = |allowed, used, reset| {
        json!({"rate_limit": {
            "allowed": allowed,
            "primary_window": {"used_percent": used, "reset_at": reset}
        }})
    };
    mount_usage(&server, usage(false, 100, reset)).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed exhaustion");
    service
        .synchronize_at(now)
        .await
        .expect("reset grace without periodic history");
    advance_clock(&mut now, 9).await;
    service
        .synchronize_at(now)
        .await
        .expect("still in reset grace");
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);
    advance_clock(&mut now, 1).await;
    service
        .synchronize_at(now)
        .await
        .expect("initial reset check");
    assert_eq!(server.received_requests().await.expect("requests").len(), 2);

    // 本次拒绝发生在 reset 之后，滞后的 allowed 不能复用旧边界解封
    mount_usage(&server, usage(true, 100, reset)).await;
    for (elapsed, requests) in [
        (29, 0),
        (1, 1),
        (29, 1),
        (1, 2),
        (229, 3),
        (1, 3),
        (298, 3),
        (1, 4),
    ] {
        advance_clock(&mut now, elapsed).await;
        service.synchronize_at(now).await.expect("scheduled check");
        assert_eq!(
            server.received_requests().await.expect("requests").len(),
            requests
        );
        assert!(
            store
                .account("acct_reset_retry")
                .expect("account")
                .quota()
                .is_exhausted()
        );
    }

    // 查询失败也占用一次尝试，避免网络故障时取消节流
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    advance_clock(&mut now, 300).await;
    assert_eq!(
        service
            .synchronize_at(now)
            .await
            .expect("failed check")
            .transient,
        1
    );
    advance_clock(&mut now, 299).await;
    service
        .synchronize_at(now)
        .await
        .expect("throttled failed check");
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);

    mount_usage(&server, usage(true, 0, reset + 18_000)).await;
    advance_clock(&mut now, 1).await;
    assert_eq!(
        service
            .synchronize_at(now)
            .await
            .expect("recovery check")
            .updated,
        1
    );
    assert!(
        !store
            .account("acct_reset_retry")
            .expect("account")
            .quota()
            .is_exhausted()
    );
    advance_clock(&mut now, 300).await;
    service
        .synchronize_at(now)
        .await
        .expect("recovered account leaves retry schedule");
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);
}

#[tokio::test]
async fn allowed_account_with_expired_window_synchronizes_at_reset_grace() {
    let mut now = SystemTime::now();
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_allowed_expired_window").await;
    let account = store
        .account("acct_allowed_expired_window")
        .expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());

    let short_reset = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 114;
    let usage = |used: u32, reset: i64| {
        json!({"rate_limit": {
            "allowed": true,
            "primary_window": {
                "used_percent": used,
                "reset_at": reset,
                "limit_window_seconds": 18_000,
            }
        }})
    };

    mount_usage(&server, usage(100, short_reset)).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed quota with 100% usage");

    assert!(
        now < reset_grace(short_reset),
        "fixture must precede grace deadline"
    );
    let requests = server.received_requests().await.expect("requests").len();
    service
        .synchronize_at(now)
        .await
        .expect("initial periodic check");
    service
        .synchronize_at(now)
        .await
        .expect("repeat check before grace");
    assert_eq!(
        server.received_requests().await.expect("requests").len(),
        requests,
        "正常账号首次复核也必须等待 reset 宽限期"
    );

    // 当到达 reset + 120s 宽限期后，账号虽然处于 allowed 状态，但包含已到期的非零用量窗口，被调度主动同步
    mount_usage(&server, usage(0, short_reset + 18_000)).await;
    now = reset_grace(short_reset);

    let summary = service
        .synchronize_at(now)
        .await
        .expect("synchronize expired window");
    assert_eq!(summary.updated, 1);
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);

    let snapshot = service
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("snapshot");
    assert_eq!(snapshot.windows()[0].used_percent(), Some(0.0));
    service
        .synchronize_at(now)
        .await
        .expect("skip fresh window");
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);
}

#[tokio::test]
async fn allowed_expired_window_refresh_is_throttled_when_observation_is_unchanged() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_allowed_expired_retry").await;
    let account = store
        .account("acct_allowed_expired_retry")
        .expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let usage = json!({"rate_limit": {
        "allowed": true,
        "primary_window": {
            "used_percent": 74,
            "reset_at": Utc::now().timestamp() - 180,
            "limit_window_seconds": 18_000,
        }
    }});
    mount_usage(&server, usage.clone()).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed quota");
    mount_usage(&server, usage).await;

    assert_eq!(service.synchronize().await.expect("first check").updated, 1);
    service.synchronize().await.expect("throttled repeat check");
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);
}
