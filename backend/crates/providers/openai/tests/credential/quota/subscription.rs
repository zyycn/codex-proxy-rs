//! 订阅展示不得随手工或后台额度检查预取

use super::*;

#[tokio::test]
async fn quota_refresh_and_background_synchronize_do_not_query_subscription() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_quota_without_subscription").await;
    let account = store.account("acct_quota_without_subscription").unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/backend-api/subscriptions"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "primary_window": { "used_percent": 20, "limit_window_seconds": 18000 }
            }
        })))
        .expect(2..)
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    service.synchronize().await.unwrap();
    service.refresh_account(account.id()).await.unwrap();
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.url.path() == "/api/codex/usage")
    );
}
