//! 注入续期结果验证终态原因、恢复重试及显式截止，不连接外部 Redis

#[path = "../src/lease_renewal.rs"]
mod implementation;

use gateway_core::lifecycle::{
    CancellationReason, CancellationToken, Deadline, LeaseFailure, REQUEST_LEASE_TTL,
    RequestLeaseKind,
};
use implementation::LeaseRenewal;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn temporary_error() -> gateway_store::StoreError {
    gateway_store::StoreError::Unavailable {
        backend: gateway_store::StoreBackend::Redis,
        message: "injected renewal failure".into(),
        source: None,
    }
}

#[tokio::test(start_paused = true)]
async fn missing_lease_and_pending_renewal_preserve_distinct_causes() {
    for kind in [
        RequestLeaseKind::ClientAdmission,
        RequestLeaseKind::Credential,
    ] {
        for pending in [false, true] {
            let token = CancellationToken::new();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let _guard = LeaseRenewal::spawn(
                Deadline::default(),
                Some((token.clone(), kind)),
                move |_| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        if pending {
                            std::future::pending().await
                        } else {
                            Ok(false)
                        }
                    })
                },
            );
            token.cancelled().await;
            token.cancel();
            assert_eq!(
                token.reason(),
                Some(CancellationReason::Lease {
                    kind,
                    failure: if pending {
                        LeaseFailure::RenewalTimedOut
                    } else {
                        LeaseFailure::Lost
                    },
                })
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn temporary_failure_recovers_without_terminal_cancellation() {
    let token = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let guard = LeaseRenewal::spawn(
        Deadline::default(),
        Some((token.clone(), RequestLeaseKind::Credential)),
        move |_| {
            let first = observed.fetch_add(1, Ordering::SeqCst) == 0;
            Box::pin(async move {
                if first {
                    Err(temporary_error())
                } else {
                    Ok(true)
                }
            })
        },
    );
    tokio::task::yield_now().await;
    for _ in 0..4 {
        tokio::time::advance(REQUEST_LEASE_TTL / 3).await;
        tokio::task::yield_now().await;
    }
    assert!(calls.load(Ordering::SeqCst) >= 4);
    assert_eq!(token.reason(), None);
    drop(guard);
}

#[tokio::test(start_paused = true)]
async fn exhausted_retry_window_retains_expiration_cause() {
    let token = CancellationToken::new();
    let _guard = LeaseRenewal::spawn(
        Deadline::default(),
        Some((token.clone(), RequestLeaseKind::ClientAdmission)),
        |_| Box::pin(async { Err(temporary_error()) }),
    );
    token.cancelled().await;
    assert_eq!(
        token.reason(),
        Some(CancellationReason::Lease {
            kind: RequestLeaseKind::ClientAdmission,
            failure: LeaseFailure::Expired,
        })
    );
}

#[tokio::test]
async fn explicit_deadline_and_observation_lease_do_not_cancel_business() {
    let token = CancellationToken::new();
    let _guard = LeaseRenewal::spawn(
        std::time::SystemTime::UNIX_EPOCH.into(),
        Some((token.clone(), RequestLeaseKind::Credential)),
        |_| panic!("elapsed request does not renew"),
    );
    let _observation =
        LeaseRenewal::spawn(Deadline::default(), None, |_| Box::pin(async { Ok(false) }));
    tokio::task::yield_now().await;
    assert_eq!(token.reason(), None);
}
