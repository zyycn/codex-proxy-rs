//! 验证轮询分配与亲和认领的原子性，以及排队、分流和插件选号的游标边界

use super::affinity::session_request;
use crate::support::{Fixture, context_with_scope, request};
use futures::future::BoxFuture;
use gateway_core::{
    account::{
        AccountAffinity, AccountSelectionPolicy, PlaintextCredential, ProviderAccountId,
        ProviderAccountStore, RotationStrategy,
    },
    concurrency::ConcurrencyQueuePolicy,
    engine::{
        AttemptContext, ModelRequestId, RequestAttemptContext,
        policy::{
            AccountScheduleDecision, AccountScheduleInput, ModelRouteDecision, ModelRouteInput,
            RequestPolicyContext, RequestPolicyFault, RequestPolicyPlan,
        },
        provider::{Provider, ProviderRequest, ProviderStream},
    },
    error::ProviderErrorKind,
    lifecycle::CancellationToken,
    policy::ClientApiKeyId,
    routing::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
};
use serde_json::json;
use std::{
    num::NonZeroU32,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

fn context(fixture: &Fixture, mode: AccountAffinity, max_concurrent: u32) -> AttemptContext {
    context_with_scope(
        RequestAttemptContext::new(
            ModelRequestId::new("req_round_robin").unwrap(),
            ClientApiKeyId::new("key_fixture").unwrap(),
        ),
        AccountSelectionPolicy::new(
            RotationStrategy::RoundRobin,
            NonZeroU32::new(max_concurrent).unwrap(),
        )
        .with_account_affinity(mode)
        .with_queue(ConcurrencyQueuePolicy {
            max_waiting: 1,
            timeout: Duration::from_secs(3),
        }),
        CancellationToken::new(),
        fixture.store.account_scope(),
    )
}

fn session(session: Option<&str>) -> ProviderRequest {
    session.map_or_else(
        || request(false),
        |session| {
            session_request(
                gateway_protocol::genai::GENAI_PROTOCOL,
                json!({"contents":[{"parts":[{"text":"hello"}]}]}),
                json!({"headers":{"x-http-session-id":session}}),
            )
        },
    )
}

async fn select(fixture: &Fixture, key: Option<&str>, mode: AccountAffinity) -> ProviderStream {
    fixture
        .provider
        .clone()
        .execute(session(key), context(fixture, mode, 1))
        .await
        .unwrap()
}

fn cursor(fixture: &Fixture) -> u64 {
    fixture
        .store
        .assignment_cursors
        .lock()
        .unwrap()
        .get(&ClientApiKeyId::new("key_fixture").unwrap())
        .copied()
        .unwrap_or_default()
}

#[tokio::test]
async fn affinity_hits_leave_the_next_new_session_its_turn() {
    for mode in [AccountAffinity::Strict, AccountAffinity::Preferred] {
        let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
        for _ in 0..4 {
            let stream = select(&fixture, Some("session-a"), mode).await;
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                "acct_fixture"
            );
            drop(stream);
            assert_eq!(cursor(&fixture), 1);
        }
        let next = select(&fixture, Some("session-b"), mode).await;
        assert_eq!(
            next.metadata().provider_account_id().as_str(),
            "acct_standby"
        );
        assert_eq!(cursor(&fixture), 2);
        drop(next);
        assert_eq!(fixture.store.active_leases(), 0);
    }
}

#[tokio::test]
async fn preferred_spillover_keeps_the_cursor_and_migration_advances_it() {
    let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
    let mode = AccountAffinity::Preferred;
    let holder = select(&fixture, Some("session-a"), mode).await;
    let primary = holder.metadata().provider_account_id().clone();
    let overflow = select(&fixture, Some("session-a"), mode).await;
    let spare = overflow.metadata().provider_account_id().clone();
    assert_ne!(spare, primary);
    assert_eq!(cursor(&fixture), 1);
    drop((holder, overflow));
    assert_eq!(
        select(&fixture, Some("session-a"), mode)
            .await
            .metadata()
            .provider_account_id(),
        &primary
    );
    assert_eq!(cursor(&fixture), 1);

    fixture.store.set_enabled(&primary, false).await.unwrap();
    assert_eq!(
        select(&fixture, Some("session-a"), mode)
            .await
            .metadata()
            .provider_account_id(),
        &spare
    );
    assert_eq!(cursor(&fixture), 2);
    fixture.store.set_enabled(&primary, true).await.unwrap();
    assert_eq!(
        select(&fixture, Some("session-a"), mode)
            .await
            .metadata()
            .provider_account_id(),
        &spare
    );
    assert_eq!(cursor(&fixture), 2);
    assert_eq!(fixture.store.active_leases(), 0);
}

#[tokio::test]
async fn queue_rechecks_do_not_advance_until_a_new_assignment_is_admitted() {
    for bound in [false, true] {
        let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
        let mode = AccountAffinity::Strict;
        let holder = select(&fixture, Some("session-a"), mode).await;
        let spare = select(&fixture, Some("session-b"), mode).await;
        let reads = fixture.store.scheduling_reads.load(Ordering::SeqCst);
        let waiting = select(
            &fixture,
            Some(if bound { "session-a" } else { "session-c" }),
            mode,
        );
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(350), &mut waiting)
                .await
                .is_err()
        );
        assert!(fixture.store.scheduling_reads.load(Ordering::SeqCst) > reads + 1);
        assert_eq!(cursor(&fixture), 2);
        drop(holder);
        let resumed = waiting.await;
        assert_eq!(
            resumed.metadata().provider_account_id().as_str(),
            "acct_fixture"
        );
        assert_eq!(cursor(&fixture), if bound { 2 } else { 3 });
        drop((resumed, spare));
        assert_eq!(fixture.store.active_leases(), 0);
    }
}

#[tokio::test]
async fn concurrent_first_claims_commit_only_the_winning_snapshot() {
    for same_session in [true, false] {
        let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
        *fixture.store.admission_barrier.lock().unwrap() =
            Some(Arc::new(tokio::sync::Barrier::new(2)));
        let execute = |key| {
            fixture.provider.clone().execute(
                session(Some(key)),
                context(&fixture, AccountAffinity::Strict, 2),
            )
        };
        let (first, second) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(
                execute("session-a"),
                execute(if same_session {
                    "session-a"
                } else {
                    "session-b"
                })
            )
        })
        .await
        .unwrap();
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(
            first.metadata().provider_account_id() == second.metadata().provider_account_id(),
            same_session
        );
        let assignments = if same_session { 1 } else { 2 };
        assert_eq!(cursor(&fixture), assignments);
        assert_eq!(
            fixture.store.bindings.lock().unwrap().len() as u64,
            assignments
        );
        assert_eq!(fixture.store.claims.load(Ordering::SeqCst), 3);
        assert_eq!(fixture.store.active_leases(), 2);
        drop((first, second));
        assert_eq!(fixture.store.active_leases(), 0);
    }
}

#[tokio::test]
async fn relaxed_and_sessionless_requests_rotate_only_after_admission() {
    for key in [None, Some("session-a")] {
        let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
        fixture
            .store
            .reject_next_claim
            .store(true, Ordering::SeqCst);
        for (index, expected) in ["acct_fixture", "acct_standby", "acct_fixture"]
            .iter()
            .enumerate()
        {
            let stream = select(&fixture, key, AccountAffinity::Relaxed).await;
            assert_eq!(stream.metadata().provider_account_id().as_str(), *expected);
            assert_eq!(cursor(&fixture), index as u64 + 1);
            drop(stream);
        }
        assert_eq!(fixture.store.claims.load(Ordering::SeqCst), 4);
        assert_eq!(
            fixture.store.bindings.lock().unwrap().len(),
            usize::from(key.is_some())
        );
        assert_eq!(fixture.store.active_leases(), 0);
    }
}

#[tokio::test]
async fn credential_validation_failure_releases_the_lease_without_committing() {
    let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
    let original = {
        let mut loaded = fixture.store.primary().loaded.lock().unwrap();
        std::mem::replace(
            &mut loaded.credential,
            PlaintextCredential::new(Default::default()),
        )
    };
    let error = fixture
        .provider
        .clone()
        .execute(
            session(Some("session-a")),
            context(&fixture, AccountAffinity::Strict, 1),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(
        error.kind(),
        ProviderErrorKind::ProviderInfrastructureUnavailable
    );
    assert_eq!(fixture.store.claims.load(Ordering::SeqCst), 0);
    assert!(fixture.store.bindings.lock().unwrap().is_empty());
    assert_eq!(cursor(&fixture), 0);
    assert_eq!(fixture.store.active_leases(), 0);

    fixture.store.primary().loaded.lock().unwrap().credential = original;
    let stream = select(&fixture, Some("session-a"), AccountAffinity::Strict).await;
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_fixture"
    );
    assert_eq!(cursor(&fixture), 1);
    drop(stream);
    assert_eq!(fixture.store.active_leases(), 0);
}

#[derive(Debug)]
struct SelectionPolicy(bool);

impl ExtensionSetLease for SelectionPolicy {
    fn is_ready(&self) -> bool {
        true
    }
}

impl RequestPolicyPlan for SelectionPolicy {
    fn route_model(
        &self,
        _: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>> {
        unreachable!("unused routing hook")
    }

    fn schedule_account(
        &self,
        _: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>> {
        let decision = if self.0 {
            AccountScheduleDecision::Pick(ProviderAccountId::new("acct_standby").unwrap())
        } else {
            AccountScheduleDecision::Delegate
        };
        Box::pin(async move { Ok(decision) })
    }
}

#[tokio::test]
async fn explicit_plugin_selection_keeps_the_cursor_while_delegation_rotates() {
    for explicit in [true, false] {
        let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
        let plan = Arc::new(SelectionPolicy(explicit));
        let id = ModelRequestId::new("req_round_robin").unwrap();
        let key = ClientApiKeyId::new("key_fixture").unwrap();
        let policy = RequestPolicyContext::new(
            plan.clone(),
            ExtensionSetReference::new(ExtensionSetId::new("fixture".into()).unwrap(), plan),
            id.clone(),
            key.clone(),
            vec![],
        );
        let ctx = context_with_scope(
            RequestAttemptContext::new(id, key).with_request_policy(Some(policy)),
            AccountSelectionPolicy::new(RotationStrategy::RoundRobin, NonZeroU32::MIN),
            CancellationToken::new(),
            fixture.store.account_scope(),
        );
        let stream = fixture
            .provider
            .clone()
            .execute(session(Some("session-a")), ctx)
            .await
            .unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            if explicit {
                "acct_standby"
            } else {
                "acct_fixture"
            }
        );
        assert_eq!(cursor(&fixture), u64::from(!explicit));
        drop(stream);
        let next = select(&fixture, Some("session-b"), AccountAffinity::Strict).await;
        assert_eq!(
            next.metadata().provider_account_id().as_str(),
            if explicit {
                "acct_fixture"
            } else {
                "acct_standby"
            }
        );
        drop(next);
        assert_eq!(fixture.store.active_leases(), 0);
    }
}
