//! 验证实时并发明细的租约生命周期、分页边界和过期成员处理

use super::*;

fn request_facts() -> gateway_core::provider_ports::ProviderLeaseMetadata {
    gateway_core::provider_ports::ProviderLeaseMetadata {
        request_id: "req_concurrency".to_owned(),
        client_key_id: gateway_core::policy::ClientApiKeyId::new("key_concurrency").unwrap(),
        client_key_name: Some("并发测试 Key".into()),
        model: Some("gpt-5.4".into()),
        client_ip: Some("2001:db8::376".parse().unwrap()),
        started_at: std::time::SystemTime::now() - Duration::from_secs(30),
    }
}

#[tokio::test]
async fn concurrency_details_follow_renewal_fencing_release_and_expiry() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let mut request = scheduling_request("acct_details", "worker", 2);
    request.ttl = Duration::from_millis(500);
    let guard = acquired(
        repository
            .try_acquire_bounded_lease_with_metadata(&request, Some(&request_facts()))
            .await
            .unwrap(),
    );
    let grant = guard.grant().unwrap().clone();
    let page = repository
        .account_concurrency(&request.resource_id, 0, 20)
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    let entry = &page.entries[0];
    assert_eq!(entry.client_key_name.as_deref(), Some("并发测试 Key"));
    assert_eq!(entry.client_ip.as_deref(), Some("2001:db8::376"));
    assert_eq!(entry.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(entry.request_id.as_deref(), Some("req_concurrency"));
    assert!(
        entry.acquired_at.unwrap() - entry.started_at.unwrap() >= chrono::Duration::seconds(29)
    );
    let lease_request = CredentialLeaseRequest {
        scope: request.scope,
        resource_id: request.resource_id.clone(),
        owner_id: request.owner_id.clone(),
        ttl: Duration::from_secs(2),
    };
    let mut stale = grant.clone();
    stale.fencing_token = gateway_store::Revision::new(grant.fencing_token.get() + 1).unwrap();
    assert!(
        !repository
            .release_credential_lease(&lease_request, &stale)
            .await
            .unwrap()
    );
    assert!(
        repository
            .renew_credential_lease(&lease_request, &stale)
            .await
            .unwrap()
            .is_none()
    );
    repository
        .renew_credential_lease(&lease_request, &grant)
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let renewed = repository
        .account_concurrency(&request.resource_id, 0, 20)
        .await
        .unwrap();
    assert_eq!(renewed.entries, page.entries);
    assert!(renewed.observed_at > page.observed_at);
    guard.release().await.unwrap();
    assert_eq!(
        repository
            .account_concurrency(&request.resource_id, 0, 20)
            .await
            .unwrap()
            .total,
        0
    );
    let details: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:*:detail:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!(details.is_empty());

    request.ttl = Duration::from_millis(50);
    let expired = acquired(
        repository
            .try_acquire_bounded_lease_with_metadata(&request, Some(&request_facts()))
            .await
            .unwrap(),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let page = repository
        .account_concurrency(&request.resource_id, 0, 20)
        .await
        .unwrap();
    assert_eq!(page.total, 0);
    assert!(page.entries.is_empty());
    assert!(!expired.release().await.unwrap());
    let details: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:*:detail:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!(details.is_empty());
}

#[tokio::test]
async fn concurrency_details_bound_deep_pages_and_ignore_expired_and_reserved_leases() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let request = scheduling_request("acct_large_details", "worker", 0);
    let guard = acquired(
        repository
            .try_acquire_bounded_lease_with_metadata(&request, Some(&request_facts()))
            .await
            .unwrap(),
    );
    let reserved = CredentialBoundedLeaseRequest {
        scope: CredentialLeaseScope::ProviderAccountReserved,
        max_concurrent: 1,
        ..request.clone()
    };
    let reserved_guard = acquired(
        repository
            .try_acquire_bounded_lease_with_metadata(&reserved, Some(&request_facts()))
            .await
            .unwrap(),
    );
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:lease:account:*:active"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(keys.len(), 1);
    let key = &keys[0];
    let now = chrono::Utc::now().timestamp_millis();
    let mut pipeline = redis::pipe();
    for index in 0..10_000 {
        pipeline
            .cmd("ZADD")
            .arg(key)
            .arg(now - 1000)
            .arg(format!("seed|expired-{index}|1"))
            .ignore();
        pipeline
            .cmd("ZADD")
            .arg(key)
            .arg(now + 120_000 + index)
            .arg(format!("seed|active-{index}|1"))
            .ignore();
    }
    pipeline.query_async::<()>(&mut connection).await.unwrap();
    let page = repository
        .account_concurrency(&request.resource_id, 9_981, 20)
        .await
        .unwrap();
    assert_eq!(page.total, 10_001);
    assert_eq!(page.entries.len(), 20);
    assert_eq!(page.entries[0].lease_id, "active-9980");
    assert_eq!(page.entries[19].lease_id, "active-9999");
    assert!(
        page.entries
            .iter()
            .all(|entry| entry.client_key_id.is_none())
    );
    assert!(
        repository
            .account_concurrency(&request.resource_id, 10_001, 20)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(
        repository
            .account_concurrency(&request.resource_id, 0, 101)
            .await
            .is_err()
    );
    assert!(
        repository
            .account_concurrency(&request.resource_id, u64::MAX, 20)
            .await
            .is_err()
    );
    let count: u64 = redis::cmd("ZCARD")
        .arg(key)
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(
        count, 20_001,
        "bounded read must not prune the entire expired interval"
    );
    guard.release().await.unwrap();
    reserved_guard.release().await.unwrap();
    redis::cmd("DEL")
        .arg(key)
        .query_async::<u64>(&mut connection)
        .await
        .unwrap();
}
