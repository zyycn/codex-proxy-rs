//! 验证单次尝试的保护反馈、原始错误保留及慢存储与取消边界

use futures::{StreamExt, future::BoxFuture, stream};
use gateway_core::{
    account::{
        AccountErrorCondition, AccountErrorField, AccountErrorOperator, AccountErrorProtection,
        AccountErrorRule, AccountProtectionMatch, CredentialRevision, ProviderAccountId,
    },
    engine::provider::{ProviderCallMetadata, ProviderStream},
    error::{ProviderError, ProviderErrorKind},
    provider_ports::{
        AccountProtectionGuard, ProviderCooldown, ProviderCooldownPort, ProviderCooldownScope,
        ProviderScopedCooldown, ProviderStoreError,
    },
    routing::{ConfigRevision, ProviderKind, UpstreamModelId},
    upstream::{UpstreamFailureFields, UpstreamSendState, UpstreamTransport},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct Feedback {
    pub(crate) publications: Mutex<Vec<u64>>,
    pub(crate) publish_fails: AtomicBool,
    pub(super) begin: AtomicUsize,
    pub(super) successes: AtomicUsize,
    pub(super) failures: Mutex<Vec<(AccountProtectionGuard, Vec<AccountProtectionMatch>)>>,
    stalled_begin: bool,
    stalled_feedback: bool,
    stale: bool,
}

impl ProviderCooldownPort for Feedback {
    fn publish_protection_policy<'a>(
        &'a self,
        _: &'a AccountErrorProtection,
        revision: ConfigRevision,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            self.publications.lock().unwrap().push(revision.get());
            if self.publish_fails.load(Ordering::SeqCst) {
                Err(ProviderStoreError::new(
                    gateway_core::provider_ports::ProviderStoreErrorKind::Unavailable,
                    "test_publish",
                ))
            } else {
                Ok(())
            }
        })
    }

    fn begin_protection<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        revision: CredentialRevision,
        fingerprint: &'a str,
        _: ConfigRevision,
    ) -> BoxFuture<'a, Result<Option<AccountProtectionGuard>, ProviderStoreError>> {
        Box::pin(async move {
            self.begin.fetch_add(1, Ordering::SeqCst);
            if self.stalled_begin {
                return futures::future::pending().await;
            }
            Ok((!self.stale).then(|| AccountProtectionGuard {
                account_id: account_id.clone(),
                credential_revision: revision,
                policy_generation: fingerprint.to_owned(),
                reset_generation: "epoch".into(),
                evidence_version: 0,
            }))
        })
    }
    fn record_protection_failure<'a>(
        &'a self,
        guard: &'a AccountProtectionGuard,
        _: &'a AccountErrorProtection,
        matched: &'a [AccountProtectionMatch],
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            if self.stalled_feedback {
                return futures::future::pending().await;
            }
            self.failures
                .lock()
                .unwrap()
                .push((guard.clone(), matched.to_vec()));
            Ok(true)
        })
    }
    fn record_protection_success<'a>(
        &'a self,
        _: &'a AccountProtectionGuard,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            self.successes.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        })
    }
    fn put_if_later(&self, _: ProviderCooldown) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn read<'a>(
        &'a self,
        _: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }
    fn clear<'a>(
        &'a self,
        _: &'a ProviderAccountId,
        _: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn put_scoped_if_later(
        &self,
        _: ProviderScopedCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn read_scoped<'a>(
        &'a self,
        _: &'a ProviderAccountId,
        _: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }
    fn clear_scoped<'a>(
        &'a self,
        _: &'a ProviderAccountId,
        _: &'a ProviderCooldownScope,
        _: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn clear_all<'a>(
        &'a self,
        _: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn clear_after_success<'a>(
        &'a self,
        _: &'a ProviderAccountId,
        _: CredentialRevision,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

pub(super) fn policy() -> Arc<AccountErrorProtection> {
    Arc::new(AccountErrorProtection {
        enabled: true,
        rules: vec![AccountErrorRule {
            id: "prompt".into(),
            name: "自定义错误".into(),
            conditions: vec![AccountErrorCondition {
                field: AccountErrorField::Code,
                operator: AccountErrorOperator::Equals,
                values: vec![Some("invalid_prompt".into())],
                ignore_case: false,
            }],
        }],
        ..AccountErrorProtection::default()
    })
}

fn metadata() -> ProviderCallMetadata {
    ProviderCallMetadata::new(
        ProviderKind::new("openai").unwrap(),
        UpstreamModelId::new("model").unwrap(),
        ProviderAccountId::new("acct_protection").unwrap(),
        UpstreamTransport::new("http_sse").unwrap(),
    )
    .with_credential_revision(CredentialRevision::new(7).unwrap())
}

fn failure(code: &str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidRequest, UpstreamSendState::Sent)
        .with_retry_prohibited()
        .with_upstream_failure_fields(UpstreamFailureFields {
            status: None,
            code: Some(code.into()),
            error_type: None,
            message: Some("original".into()),
        })
}

#[test]
fn custom_rule_records_one_failure_without_changing_error_or_replay() {
    futures::executor::block_on(async {
        let store = Arc::new(Feedback::default());
        let mut policy = policy().as_ref().clone();
        let mut other = policy.rules[0].clone();
        other.id = "second".into();
        policy.rules.push(other);
        let mut stream = ProviderStream::new(
            metadata(),
            stream::iter([Err(failure("invalid_prompt"))]),
            (),
        )
        .with_account_error_protection(store.clone(), Arc::new(policy.compile()), 5)
        .await;
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
        assert!(error.retry_is_prohibited());
        assert_eq!(error.upstream_failure_fields().unwrap().status, None);
        assert!(stream.next().await.is_none());
        assert!(stream.next().await.is_none());
        let records = store.failures.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].1.len(), 2);
        assert_eq!(
            records[0].0.credential_revision,
            CredentialRevision::new(7).unwrap()
        );
        assert_eq!(store.successes.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn local_unmatched_and_stale_attempt_errors_do_not_count() {
    futures::executor::block_on(async {
        for (error, stale) in [
            (
                ProviderError::new(ProviderErrorKind::InvalidRequest, UpstreamSendState::Sent)
                    .with_status(400),
                false,
            ),
            (failure("other"), false),
            (failure("invalid_prompt"), true),
        ] {
            let store = Arc::new(Feedback {
                stale,
                ..Feedback::default()
            });
            let mut stream = ProviderStream::new(metadata(), stream::iter([Err(error)]), ())
                .with_account_error_protection(store.clone(), Arc::new(policy().compile()), 5)
                .await;
            assert!(stream.next().await.unwrap().is_err());
            assert!(store.failures.lock().unwrap().is_empty());
            assert_eq!(store.successes.load(Ordering::SeqCst), 0);
        }
    });
}

#[test]
fn disabled_protection_performs_no_store_io() {
    futures::executor::block_on(async {
        let store = Arc::new(Feedback::default());
        let mut stream = ProviderStream::new(
            metadata(),
            stream::iter([Err(failure("invalid_prompt"))]),
            (),
        )
        .with_account_error_protection(
            store.clone(),
            Arc::new(AccountErrorProtection::default().compile()),
            5,
        )
        .await;
        assert!(stream.next().await.unwrap().is_err());
        assert_eq!(store.begin.load(Ordering::SeqCst), 0);
        assert!(store.failures.lock().unwrap().is_empty());
    });
}

#[test]
fn complete_success_is_reported_once_but_unfinished_drop_is_not_success() {
    futures::executor::block_on(async {
        let store = Arc::new(Feedback::default());
        let unfinished = ProviderStream::new(metadata(), stream::pending(), ())
            .with_account_error_protection(store.clone(), Arc::new(policy().compile()), 5)
            .await;
        drop(unfinished);
        assert_eq!(store.successes.load(Ordering::SeqCst), 0);
        let response = gateway_core::event::ResponseMeta::new("response", "model");
        let events = stream::iter([
            Ok(gateway_core::event::GatewayEvent::Started(response.clone()).into()),
            Ok(gateway_core::event::GatewayEvent::Completed(response).into()),
        ]);
        let mut completed = ProviderStream::new(metadata(), events, ())
            .with_account_error_protection(store.clone(), Arc::new(policy().compile()), 5)
            .await;
        assert!(completed.next().await.unwrap().is_ok());
        assert_eq!(store.successes.load(Ordering::SeqCst), 0);
        assert!(completed.next().await.unwrap().is_ok());
        assert_eq!(store.successes.load(Ordering::SeqCst), 1);
        assert!(completed.next().await.is_none());
        assert!(completed.next().await.is_none());
        assert_eq!(store.successes.load(Ordering::SeqCst), 1);
    });
}

struct Lease(Arc<AtomicUsize>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn stalled_begin_and_feedback_are_bounded_and_preserve_original_failure() {
    futures::executor::block_on(async {
        for stalled_begin in [false, true] {
            let store = Arc::new(Feedback {
                stalled_begin,
                stalled_feedback: true,
                ..Feedback::default()
            });
            let released = Arc::new(AtomicUsize::new(0));
            let started = Instant::now();
            let mut stream = ProviderStream::new(
                metadata(),
                stream::iter([Err(failure("invalid_prompt"))]),
                Lease(released.clone()),
            )
            .with_account_error_protection(store, Arc::new(policy().compile()), 5)
            .await;
            let error = timeout(Duration::from_secs(1), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
            assert!(started.elapsed() < Duration::from_millis(500));
            drop(stream);
            assert_eq!(released.load(Ordering::SeqCst), 1);
        }
    });
}

#[test]
fn cancelling_pending_feedback_releases_lease_and_does_not_report_success() {
    futures::executor::block_on(async {
        let store = Arc::new(Feedback {
            stalled_feedback: true,
            ..Feedback::default()
        });
        let released = Arc::new(AtomicUsize::new(0));
        let mut stream = ProviderStream::new(
            metadata(),
            stream::iter([Err(failure("invalid_prompt"))]),
            Lease(released.clone()),
        )
        .with_account_error_protection(store.clone(), Arc::new(policy().compile()), 5)
        .await;
        assert!(
            timeout(Duration::from_millis(10), stream.next())
                .await
                .is_err()
        );
        drop(stream);
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(store.successes.load(Ordering::SeqCst), 0);
    });
}

async fn timeout<T>(
    duration: Duration,
    future: impl std::future::Future<Output = T>,
) -> Result<T, ()> {
    let future = Box::pin(future);
    match futures::future::select(future, futures_timer::Delay::new(duration)).await {
        futures::future::Either::Left((value, _)) => Ok(value),
        futures::future::Either::Right(_) => Err(()),
    }
}
