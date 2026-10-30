//! 验证互联同步计划的并发领取、租约回收、快照保留与连接写入围栏

use super::*;
use gateway_core::account::{
    UpstreamConnection, UpstreamInfoClaim, UpstreamInfoCompletion, UpstreamInfoFailure,
    UpstreamInfoStore,
};

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

async fn fixture(
    label: &str,
) -> Option<(
    TestDatabase,
    PgProviderAccountRepository,
    UpstreamConnection,
)> {
    let database = TestDatabase::create(label).await?;
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let mut input = account("acct_peer", "peer-fixture");
    input.authentication_kind = "api_key".into();
    repository.insert_provider_account(input).await.unwrap();
    let loaded = repository
        .load_current_credential(&ProviderAccountId::new("acct_peer").unwrap())
        .await
        .unwrap();
    let connection = UpstreamConnection::for_account(&loaded.account);
    repository
        .reconcile(&connection, true, now())
        .await
        .unwrap();
    Some((database, repository, connection))
}

fn observation() -> OpaqueProviderData {
    OpaqueProviderData::new(
        json!({"fixture": "unchanged observation"})
            .as_object()
            .unwrap()
            .clone(),
    )
}

fn completion(
    claim: &UpstreamInfoClaim,
    at: u64,
    result: Result<OpaqueProviderData, UpstreamInfoFailure>,
) -> UpstreamInfoCompletion {
    UpstreamInfoCompletion {
        connection: claim.plan.connection.clone(),
        request_generation: claim.plan.request_generation,
        claim_token: claim.claim_token,
        completed_at: now() + Duration::from_secs(at),
        next_attempt_at: now() + Duration::from_secs(at + 300),
        result,
    }
}

#[tokio::test]
async fn concurrent_claim_and_expired_lease_allow_only_the_current_generation_to_complete() {
    let Some((database, repository, connection)) = fixture("peer_claim").await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let (first, second) = tokio::join!(
        repository.claim_due(&provider, now(), 20, Duration::from_secs(5)),
        repository.claim_due(&provider, now(), 20, Duration::from_secs(5)),
    );
    let claimed = first
        .unwrap()
        .into_iter()
        .chain(second.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(claimed.len(), 1);
    let recovered = repository
        .claim_due(
            &provider,
            now() + Duration::from_secs(6),
            20,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert!(recovered[0].plan.request_generation > claimed[0].plan.request_generation);
    assert!(
        !repository
            .complete(completion(&claimed[0], 7, Ok(observation())))
            .await
            .unwrap()
    );
    assert!(
        repository
            .complete(completion(&recovered[0], 7, Ok(observation())))
            .await
            .unwrap()
    );
    assert!(
        !repository
            .complete(completion(&recovered[0], 8, Ok(observation())))
            .await
            .unwrap()
    );
    let restarted = PgProviderAccountRepository::new(database.pool.clone());
    let saved = restarted
        .read(&[connection.account_id])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(saved.snapshot, Some(observation()));
    assert_eq!(
        saved.plan.completed_generation,
        recovered[0].plan.request_generation
    );
    database.close().await;
}

#[tokio::test]
async fn recreated_plan_rejects_a_previous_claim_even_when_its_generation_matches() {
    let Some((database, repository, connection)) = fixture("peer_recreated").await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let previous = repository
        .claim_due(&provider, now(), 1, Duration::from_secs(60))
        .await
        .unwrap()
        .remove(0);
    repository
        .set_provider_account_enabled(connection.account_id.as_str(), false)
        .await
        .unwrap();
    repository
        .reconcile(&connection, false, now())
        .await
        .unwrap();
    repository
        .set_provider_account_enabled(connection.account_id.as_str(), true)
        .await
        .unwrap();
    repository
        .reconcile(&connection, true, now())
        .await
        .unwrap();
    let current = repository
        .claim_due(&provider, now(), 1, Duration::from_secs(60))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        previous.plan.request_generation,
        current.plan.request_generation
    );
    assert!(
        !repository
            .complete(completion(&previous, 1, Ok(observation())))
            .await
            .unwrap()
    );
    assert!(
        repository
            .complete(completion(&current, 2, Ok(observation())))
            .await
            .unwrap()
    );
    database.close().await;
}

#[tokio::test]
async fn queued_and_claimed_refreshes_preserve_the_last_large_observation() {
    let Some((database, repository, connection)) = fixture("peer_plan_reads").await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let snapshot = OpaqueProviderData::new(
        json!({"payload": "x".repeat(2 * 1024 * 1024)})
            .as_object()
            .unwrap()
            .clone(),
    );
    let first = repository
        .claim_due(&provider, now(), 1, Duration::from_secs(60))
        .await
        .unwrap()
        .remove(0);
    assert!(
        repository
            .complete(completion(&first, 1, Ok(snapshot.clone())))
            .await
            .unwrap()
    );
    let queued = repository
        .request_refresh(
            &connection,
            now() + Duration::from_secs(2),
            Duration::from_secs(15),
        )
        .await
        .unwrap();
    let pending = repository
        .read_plans(std::slice::from_ref(&connection.account_id))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(pending, queued);
    let claimed = repository
        .claim_due(
            &provider,
            queued.next_attempt_at,
            1,
            Duration::from_secs(60),
        )
        .await
        .unwrap()
        .remove(0);
    assert_eq!(claimed.plan.request_generation, queued.request_generation);
    let saved = repository
        .read(&[connection.account_id])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(saved.snapshot, Some(snapshot));
    assert_eq!(
        saved.plan.last_success_at,
        Some(now() + Duration::from_secs(1))
    );
    database.close().await;
}

#[tokio::test]
async fn refresh_deduplicates_preserves_failure_backoff_and_clears_revoked_observations() {
    let Some((database, repository, connection)) = fixture("peer_refresh").await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let first = repository
        .claim_due(&provider, now(), 1, Duration::from_secs(60))
        .await
        .unwrap()
        .remove(0);
    repository
        .complete(completion(&first, 1, Ok(observation())))
        .await
        .unwrap();
    let requested = repository
        .request_refresh(
            &connection,
            now() + Duration::from_secs(2),
            Duration::from_secs(15),
        )
        .await
        .unwrap();
    assert_eq!(requested.next_attempt_at, now() + Duration::from_secs(15));
    let duplicate = repository
        .request_refresh(
            &connection,
            now() + Duration::from_secs(3),
            Duration::from_secs(15),
        )
        .await
        .unwrap();
    assert_eq!(requested.request_generation, duplicate.request_generation);
    let second = repository
        .claim_due(
            &provider,
            requested.next_attempt_at,
            1,
            Duration::from_secs(60),
        )
        .await
        .unwrap()
        .remove(0);
    repository
        .complete(completion(
            &second,
            16,
            Err(UpstreamInfoFailure::Unavailable),
        ))
        .await
        .unwrap();
    let retained = repository
        .read(std::slice::from_ref(&connection.account_id))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(retained.snapshot, Some(observation()));
    assert_eq!(
        retained.plan.last_success_at,
        Some(now() + Duration::from_secs(1))
    );
    let retry = repository
        .request_refresh(
            &connection,
            now() + Duration::from_secs(17),
            Duration::from_secs(15),
        )
        .await
        .unwrap();
    assert_eq!(retry.next_attempt_at, retained.plan.next_attempt_at);
    assert!(
        repository
            .claim_due(
                &provider,
                now() + Duration::from_secs(30),
                1,
                Duration::from_secs(60)
            )
            .await
            .unwrap()
            .is_empty()
    );
    let third = repository
        .claim_due(&provider, retry.next_attempt_at, 1, Duration::from_secs(60))
        .await
        .unwrap()
        .remove(0);
    repository
        .complete(completion(
            &third,
            317,
            Err(UpstreamInfoFailure::Unauthorized),
        ))
        .await
        .unwrap();
    let revoked = repository
        .read(std::slice::from_ref(&connection.account_id))
        .await
        .unwrap()
        .remove(0);
    assert!(revoked.snapshot.is_none());
    assert_eq!(
        revoked.plan.last_error,
        Some(UpstreamInfoFailure::Unauthorized)
    );
    database.close().await;
}

#[tokio::test]
async fn changed_credentials_and_proxy_reject_old_results_before_reconciliation() {
    let Some((database, repository, original)) = fixture("peer_connection").await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let first = repository
        .claim_due(&provider, now(), 1, Duration::from_secs(60))
        .await
        .unwrap()
        .remove(0);
    sqlx::query(
        "update provider_accounts set credential_revision = credential_revision + 1 where id = $1",
    )
    .bind(original.account_id.as_str())
    .execute(&database.pool)
    .await
    .unwrap();
    assert!(
        !repository
            .complete(completion(&first, 1, Ok(observation())))
            .await
            .unwrap()
    );
    assert_eq!(
        repository
            .request_refresh(&original, now(), Duration::ZERO)
            .await
            .unwrap_err()
            .kind(),
        gateway_core::error::StoreErrorKind::Conflict
    );
    let loaded = repository
        .load_current_credential(&original.account_id)
        .await
        .unwrap();
    let rotated = UpstreamConnection::for_account(&loaded.account);
    repository.reconcile(&rotated, true, now()).await.unwrap();
    let second = repository
        .claim_due(&provider, now(), 1, Duration::from_secs(60))
        .await
        .unwrap()
        .remove(0);
    assert!(second.plan.request_generation > first.plan.request_generation);
    sqlx::query(
        "update provider_accounts set outbound_proxy_url = 'http://127.0.0.1:8123' where id = $1",
    )
    .bind(original.account_id.as_str())
    .execute(&database.pool)
    .await
    .unwrap();
    assert!(
        !repository
            .complete(completion(&second, 2, Ok(observation())))
            .await
            .unwrap()
    );
    let loaded = repository
        .load_current_credential(&original.account_id)
        .await
        .unwrap();
    let changed_proxy = UpstreamConnection::for_account(&loaded.account);
    repository
        .reconcile(&changed_proxy, true, now())
        .await
        .unwrap();
    let third = repository
        .claim_due(&provider, now(), 1, Duration::from_secs(60))
        .await
        .unwrap()
        .remove(0);
    assert!(
        repository
            .complete(completion(&third, 3, Ok(observation())))
            .await
            .unwrap()
    );
    repository
        .reconcile(&changed_proxy, false, now())
        .await
        .unwrap();
    assert!(
        repository
            .read(&[original.account_id])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !repository
            .complete(completion(&third, 4, Ok(observation())))
            .await
            .unwrap()
    );
    database.close().await;
}

#[tokio::test]
async fn discovery_filters_by_declared_provider_and_authentication_kinds() {
    let Some((database, repository, _)) = fixture("peer_discovery").await else {
        return;
    };
    let mut input = account("acct_peer_xai", "generic-peer");
    input.provider_kind = "xai".into();
    input.authentication_kind = "peer_test".into();
    repository.insert_provider_account(input).await.unwrap();
    let xai = ProviderKind::new("xai").unwrap();
    let candidates = repository
        .list_candidates(&xai, &["peer_test"], None, 100)
        .await
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].account.id().as_str(), "acct_peer_xai");
    assert!(
        repository
            .list_candidates(&xai, &[], None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .list_candidates(&xai, &["api_key"], None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .list_candidates(&xai, &["peer_test"], Some(candidates[0].account.id()), 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .list_candidates(
                &ProviderKind::new("openai").unwrap(),
                &["peer_test"],
                None,
                100
            )
            .await
            .unwrap()
            .is_empty()
    );
    database.close().await;
}
