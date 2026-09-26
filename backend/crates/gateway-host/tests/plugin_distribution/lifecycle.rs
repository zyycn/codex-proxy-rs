use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use gateway_admin::{
    model::{
        AdminErrorKind,
        plugins::distribution::{DownloadPurpose, GithubReleaseQuery},
    },
    ports::plugins::PluginDistribution,
};
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

use super::{credential, digest, transport};

fn query(repository: &str) -> GithubReleaseQuery {
    GithubReleaseQuery {
        repository: repository.into(),
        tag: None,
        allow_prerelease: false,
    }
}

fn release() -> serde_json::Value {
    json!({
        "tag_name":"v1.0.0", "draft":false, "prerelease":false,
        "assets":[{
            "id":1, "name":"example_1.0.0_linux_x86_64.tar.gz", "size":7,
            "digest":format!("sha256:{}", digest(b"package"))
        }]
    })
}

async fn wait_for_requests(server: &MockServer, count: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while server.received_requests().await.unwrap().len() < count {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("查询必须实际到达受控来源");
}

#[tokio::test]
async fn cancelling_the_fetch_owner_releases_the_shared_slot_for_the_next_query() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let response_calls = calls.clone();
    Mock::given(path("/repos/example/plugins/releases/latest"))
        .respond_with(move |_: &wiremock::Request| {
            let response = ResponseTemplate::new(200).set_body_json(release());
            if response_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                response.set_delay(Duration::from_secs(30))
            } else {
                response
            }
        })
        .expect(3)
        .mount(&server)
        .await;
    let distribution = Arc::new(transport(&server));
    let first_distribution = distribution.clone();
    let first = tokio::spawn(async move {
        first_distribution
            .query_release(query("example/plugins"), vec![], None)
            .await
    });
    // 以来源确实收到请求为屏障，避免只取消尚未执行的任务而误报释放成功。
    wait_for_requests(&server, 1).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());

    let recovered = tokio::time::timeout(
        Duration::from_secs(2),
        distribution.query_release(query("example/plugins"), vec![], None),
    )
    .await
    .expect("取消不能留下占用的同源查询锁或下载容量")
    .unwrap();
    assert_eq!(recovered.tag, "v1.0.0");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let refreshed = distribution
        .query_release(query("example/plugins"), vec![], None)
        .await
        .unwrap();
    assert!(refreshed.queried_at > recovered.queried_at);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn cancelling_a_cache_waiter_does_not_cancel_or_poison_the_fetch_owner() {
    let server = MockServer::start().await;
    Mock::given(path("/repos/example/plugins/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(release())
                .set_delay(Duration::from_millis(500)),
        )
        .expect(2)
        .mount(&server)
        .await;
    let distribution = Arc::new(transport(&server));
    let first_distribution = distribution.clone();
    let first = tokio::spawn(async move {
        first_distribution
            .query_release(query("example/plugins"), vec![], None)
            .await
    });
    wait_for_requests(&server, 1).await;
    let waiter = distribution.query_release(query("example/plugins"), vec![], None);
    // timeout 会实际轮询等待者再取消；不会只中止一个未调度的 spawn。
    assert!(
        tokio::time::timeout(Duration::from_millis(25), waiter)
            .await
            .is_err()
    );
    let first = tokio::time::timeout(Duration::from_secs(2), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.tag, "v1.0.0");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    // 等待者取消不影响持有者完成，也不阻止之后的显式查询刷新结果。
    let refreshed = distribution
        .query_release(query("example/plugins"), vec![], None)
        .await
        .unwrap();
    assert!(refreshed.queried_at > first.queried_at);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn cancelling_all_active_queries_releases_the_bounded_download_capacity() {
    let server = MockServer::start().await;
    for repository in ["first/plugin", "second/plugin"] {
        Mock::given(path(format!("/repos/{repository}/releases/latest")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(release())
                    .set_delay(Duration::from_secs(30)),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(path("/repos/recovered/plugin/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release()))
        .expect(1)
        .mount(&server)
        .await;
    let distribution = Arc::new(transport(&server));
    let mut active = Vec::new();
    for repository in ["first/plugin", "second/plugin"] {
        let distribution = distribution.clone();
        active.push(tokio::spawn(async move {
            distribution
                .query_release(query(repository), vec![], None)
                .await
        }));
    }
    wait_for_requests(&server, 2).await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(25),
            distribution.query_release(query("recovered/plugin"), vec![], None),
        )
        .await
        .is_err()
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    for task in &active {
        task.abort();
    }
    for task in active {
        assert!(task.await.unwrap_err().is_cancelled());
    }
    let recovered = tokio::time::timeout(
        Duration::from_secs(2),
        distribution.query_release(query("recovered/plugin"), vec![], None),
    )
    .await
    .expect("取消必须归还全局下载容量，等待者取消也不能污染缓存")
    .unwrap();
    assert_eq!(recovered.tag, "v1.0.0");
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn rate_limit_expiry_releases_the_identity_without_reusing_a_failure_cache_entry() {
    let server = MockServer::start().await;
    Mock::given(path("/repos/limited/plugin/releases/latest"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/repos/recovered/plugin/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/repos/authorized/plugin/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release()))
        .expect(1)
        .mount(&server)
        .await;
    let distribution = transport(&server);
    assert_eq!(
        distribution
            .query_release(query("limited/plugin"), vec![], None)
            .await
            .unwrap_err()
            .kind(),
        AdminErrorKind::RateLimited
    );
    assert_eq!(
        distribution
            .query_release(query("blocked/plugin"), vec![], None)
            .await
            .unwrap_err()
            .kind(),
        AdminErrorKind::RateLimited
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    let mut authorized = credential(&server, "/repos");
    authorized.info.purposes = vec![DownloadPurpose::Metadata];
    distribution
        .query_release(query("authorized/plugin"), vec![authorized], None)
        .await
        .unwrap();
    // 同一查询另有失败缓存；改用尚未查询的仓库，只验证共享身份退避的真实到期。
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let recovered = distribution
        .query_release(query("recovered/plugin"), vec![], None)
        .await
        .unwrap();
    assert_eq!(recovered.tag, "v1.0.0");
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    assert_eq!(
        distribution
            .query_release(query("limited/plugin"), vec![], None)
            .await
            .unwrap_err()
            .kind(),
        AdminErrorKind::RateLimited
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}
