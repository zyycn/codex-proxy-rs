//! 自动重置的持久幂等恢复、低频发现与账号独立错峰

use super::*;
use gateway_admin::{
    model::reset_credit_automation::*,
    ports::reset_credit_automation::ResetCreditAutomationStore as _,
};

fn enable_capability(provider: &FakeProviderAdmin) {
    let mut capabilities = provider.capabilities.lock().unwrap();
    capabilities.reset_credits = true;
    capabilities.consume_reset_credit = true;
}

fn plan(expires_at: chrono::DateTime<Utc>) -> ResetCreditAutomation {
    ResetCreditAutomation {
        account_id: "acct_test".into(),
        enabled: true,
        revision: 1,
        next_run_at: Utc::now(),
        state: ResetCreditAutomationState {
            next_discovery_at: Utc::now() + TimeDelta::days(1),
            credits: vec![PlannedResetCredit {
                id: "card_1".into(),
                expires_at,
            }],
            pending: None,
            retry_at: None,
            failures: 0,
            last_result: None,
        },
    }
}

fn card(
    expires_at: chrono::DateTime<Utc>,
) -> gateway_admin::model::provider_credentials::ProviderResetCredit {
    gateway_admin::model::provider_credentials::ProviderResetCredit {
        id: "card_1".into(),
        status: Some("available".into()),
        expires_at: Some(expires_at),
        title: None,
        reset_type: None,
    }
}

#[tokio::test]
async fn automation_restores_pending_idempotency_after_service_restart() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    enable_capability(&provider);
    let store = FakeAccountStore::new("openai", events.clone());
    let expiry = Utc::now() + TimeDelta::minutes(5);
    provider.reset_credits.lock().unwrap().push(card(expiry));
    *provider.reset_credit_failure.lock().unwrap() = Some(ProviderAdminErrorKind::Ambiguous);
    store
        .automations
        .lock()
        .unwrap()
        .insert("acct_test".into(), plan(expiry));
    let account_id = ProviderAccountId::new("acct_test").unwrap();
    let services = accounts_service(provider.clone(), store.clone()).await;
    services
        .accounts()
        .reconcile_reset_credit_automation(account_id.clone())
        .await
        .expect_err("unknown consume result");
    let persisted = store
        .reset_credit_automation("acct_test")
        .await
        .unwrap()
        .unwrap();
    let pending = persisted.state.pending.as_ref().unwrap();
    let command = provider.reset_credit_commands()[0].clone();
    assert_eq!(
        pending.redeem_request_id,
        command.redeem_request_id.to_string()
    );
    assert_eq!(command.redeem_request_id.get_version_num(), 4);
    assert_eq!(provider.quota_requests().len(), 0);
    services
        .accounts()
        .reconcile_reset_credit_automation(account_id.clone())
        .await
        .unwrap();
    assert_eq!(
        provider.reset_credit_commands().len(),
        1,
        "wait until persisted retry time"
    );
    store
        .automations
        .lock()
        .unwrap()
        .get_mut("acct_test")
        .unwrap()
        .state
        .retry_at = Some(Utc::now() - TimeDelta::seconds(1));
    drop(services);
    let restarted = accounts_service(provider.clone(), store.clone()).await;
    restarted
        .accounts()
        .reconcile_reset_credit_automation(account_id)
        .await
        .unwrap();
    assert_eq!(provider.reset_credit_commands(), [command.clone(), command]);
    assert_eq!(
        recorded(&events)
            .iter()
            .filter(|e| **e == "provider.reset_credits")
            .count(),
        1
    );
    let record = store
        .reset_credit_automation("acct_test")
        .await
        .unwrap()
        .unwrap();
    assert!(record.state.pending.is_none());
    assert!(record.state.credits.is_empty());
    assert_eq!(record.state.last_result.as_deref(), Some("reset"));
    assert!(provider.quota_requests().iter().any(|q| q.refresh));
}

#[tokio::test]
async fn automation_only_queries_due_accounts_and_rechecks_credit_before_consume() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    enable_capability(&provider);
    let store = FakeAccountStore::new("openai", events.clone());
    let mut record = plan(Utc::now() + TimeDelta::hours(6));
    store
        .automations
        .lock()
        .unwrap()
        .insert("acct_test".into(), record.clone());
    let services = accounts_service(provider.clone(), store.clone()).await;
    let account_id = ProviderAccountId::new("acct_test").unwrap();
    services
        .accounts()
        .reconcile_reset_credit_automation(account_id.clone())
        .await
        .unwrap();
    assert!(!recorded(&events).contains(&"provider.reset_credits"));
    record.state.credits[0].expires_at = Utc::now() + TimeDelta::minutes(5);
    store
        .automations
        .lock()
        .unwrap()
        .insert("acct_test".into(), record);
    services
        .accounts()
        .reconcile_reset_credit_automation(account_id)
        .await
        .unwrap();
    assert!(recorded(&events).contains(&"provider.reset_credits"));
    assert!(
        provider.reset_credit_commands().is_empty(),
        "disappeared credit must never be consumed"
    );
}

#[tokio::test]
async fn automation_defaults_off_and_disabled_pending_survives_reenable() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    enable_capability(&provider);
    let store = FakeAccountStore::new("openai", events.clone());
    let services = accounts_service(provider.clone(), store.clone()).await;
    let account_id = ProviderAccountId::new("acct_test").unwrap();
    assert!(
        !services
            .accounts()
            .reset_credit_automation_status(account_id.clone())
            .await
            .unwrap()
            .enabled
    );
    services
        .accounts()
        .set_reset_credit_automation(&context("enable"), account_id.clone(), true)
        .await
        .unwrap();
    let first = store
        .reset_credit_automation("acct_test")
        .await
        .unwrap()
        .unwrap();
    assert!(first.state.next_discovery_at >= Utc::now() - TimeDelta::seconds(1));
    let pending = PendingResetCredit {
        credit: PlannedResetCredit {
            id: "pending".into(),
            expires_at: Utc::now() + TimeDelta::hours(1),
        },
        redeem_request_id: uuid::Uuid::new_v4().to_string(),
    };
    store
        .automations
        .lock()
        .unwrap()
        .get_mut("acct_test")
        .unwrap()
        .state
        .pending = Some(pending.clone());
    services
        .accounts()
        .set_reset_credit_automation(&context("disable"), account_id.clone(), false)
        .await
        .unwrap();
    services
        .accounts()
        .reconcile_reset_credit_automation(account_id.clone())
        .await
        .unwrap();
    assert!(provider.reset_credit_commands().is_empty());
    services
        .accounts()
        .set_reset_credit_automation(&context("reenable"), account_id, true)
        .await
        .unwrap();
    let next = store
        .reset_credit_automation("acct_test")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.state.pending, Some(pending));
    assert_eq!(next.state.next_discovery_at, first.state.next_discovery_at);
}

#[tokio::test]
async fn automation_schedules_each_account_inside_the_lead_window_and_next_local_day() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    enable_capability(&provider);
    let store = FakeAccountStore::new("openai", events);
    let now = Utc::now();
    let expiry = now + TimeDelta::days(2);
    provider.reset_credits.lock().unwrap().push(card(expiry));
    let accounts = (0..100)
        .map(|n| {
            let mut account = account_record("openai");
            account.id = format!("acct_{n}");
            let mut record = plan(expiry);
            record.account_id = account.id.clone();
            record.state.next_discovery_at = now - TimeDelta::seconds(1);
            store
                .automations
                .lock()
                .unwrap()
                .insert(account.id.clone(), record);
            account
        })
        .collect::<Vec<_>>();
    store.set_accounts(accounts.clone());
    let services = accounts_service(provider, store.clone()).await;
    let timezone = gateway_core::time::DeploymentTimeZone::default();
    let start = timezone.days_after(now, 1).unwrap();
    let end = timezone.days_after(now, 2).unwrap();
    let mut times = std::collections::BTreeSet::new();
    for account in accounts {
        let id = ProviderAccountId::new(account.id.clone()).unwrap();
        services
            .accounts()
            .reconcile_reset_credit_automation(id.clone())
            .await
            .unwrap();
        let status = services
            .accounts()
            .reset_credit_automation_status(id)
            .await
            .unwrap();
        let discovery = status.next_discovery_at.unwrap();
        assert!(discovery >= start && discovery < end);
        times.insert(status.next_consume_at.unwrap());
    }
    assert!(
        times.len() > 30,
        "accounts must not all fire at the same instant"
    );
    assert!(times.iter().all(
        |due| *due >= expiry - TimeDelta::minutes(10) && *due < expiry - TimeDelta::minutes(9)
    ));
}

#[tokio::test]
async fn automation_discards_externally_used_credit_and_stops_on_already_redeemed() {
    for already_redeemed_after_query in [false, true] {
        let events = events();
        let provider = FakeProviderAdmin::new("openai", events.clone());
        enable_capability(&provider);
        let store = FakeAccountStore::new("openai", events.clone());
        let expiry = Utc::now() + TimeDelta::minutes(5);
        let mut remote_credit = card(expiry);
        if !already_redeemed_after_query {
            remote_credit.status = Some("redeemed".into());
        }
        provider.reset_credits.lock().unwrap().push(remote_credit);
        *provider.reset_credit_result.lock().unwrap() = "already_redeemed".into();
        store
            .automations
            .lock()
            .unwrap()
            .insert("acct_test".into(), plan(expiry));
        let services = accounts_service(provider.clone(), store.clone()).await;
        let account_id = ProviderAccountId::new("acct_test").unwrap();
        services
            .accounts()
            .reconcile_reset_credit_automation(account_id.clone())
            .await
            .unwrap();
        services
            .accounts()
            .reconcile_reset_credit_automation(account_id)
            .await
            .unwrap();
        assert_eq!(
            provider.reset_credit_commands().len(),
            usize::from(already_redeemed_after_query)
        );
        let stored = store
            .reset_credit_automation("acct_test")
            .await
            .unwrap()
            .unwrap();
        assert!(stored.state.credits.is_empty());
        assert!(stored.state.pending.is_none());
        if already_redeemed_after_query {
            assert_eq!(
                stored.state.last_result.as_deref(),
                Some("already_redeemed")
            );
        }
    }
}

#[tokio::test]
async fn automation_daily_discovery_is_not_polled_again_after_success_or_failure() {
    for failed in [false, true] {
        let events = events();
        let provider = FakeProviderAdmin::new("openai", events.clone());
        enable_capability(&provider);
        if failed {
            provider.fail_next(ProviderAdminErrorKind::Unavailable);
        }
        let store = FakeAccountStore::new("openai", events.clone());
        let mut record = plan(Utc::now() + TimeDelta::days(3));
        record.state.credits.clear();
        record.state.next_discovery_at = Utc::now() - TimeDelta::seconds(1);
        store
            .automations
            .lock()
            .unwrap()
            .insert("acct_test".into(), record);
        let services = accounts_service(provider.clone(), store.clone()).await;
        let id = ProviderAccountId::new("acct_test").unwrap();
        let first = services
            .accounts()
            .reconcile_reset_credit_automation(id.clone())
            .await;
        assert_eq!(first.is_err(), failed);
        let scheduled = store
            .reset_credit_automation("acct_test")
            .await
            .unwrap()
            .unwrap()
            .state
            .next_discovery_at;
        services
            .accounts()
            .reconcile_reset_credit_automation(id)
            .await
            .unwrap();
        assert_eq!(
            recorded(&events)
                .iter()
                .filter(|e| **e == "provider.reset_credits")
                .count(),
            1
        );
        assert_eq!(
            store
                .reset_credit_automation("acct_test")
                .await
                .unwrap()
                .unwrap()
                .state
                .next_discovery_at,
            scheduled
        );
        assert!(scheduled > Utc::now());
    }
}

#[tokio::test]
async fn automation_cancelled_discovery_preserves_next_random_time_across_restart() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    enable_capability(&provider);
    let store = FakeAccountStore::new("openai", events.clone());
    let mut record = plan(Utc::now() + TimeDelta::days(3));
    record.state.credits.clear();
    record.state.next_discovery_at = Utc::now() - TimeDelta::seconds(1);
    store
        .automations
        .lock()
        .unwrap()
        .insert("acct_test".into(), record);
    let (release, gate) = tokio::sync::oneshot::channel();
    *provider.reset_credit_query_gate.lock().unwrap() = Some(gate);
    let services = accounts_service(provider.clone(), store.clone()).await;
    let task = tokio::spawn(async move {
        services
            .accounts()
            .reconcile_reset_credit_automation(ProviderAccountId::new("acct_test").unwrap())
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.reset_credit_query_started.notified(),
    )
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(release);
    let before = store
        .reset_credit_automation("acct_test")
        .await
        .unwrap()
        .unwrap();
    assert!(before.state.next_discovery_at > Utc::now());
    let restarted = accounts_service(provider.clone(), store.clone()).await;
    restarted
        .accounts()
        .reconcile_reset_credit_automation(ProviderAccountId::new("acct_test").unwrap())
        .await
        .unwrap();
    let after = store
        .reset_credit_automation("acct_test")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        before.state.next_discovery_at,
        after.state.next_discovery_at
    );
    assert_eq!(
        recorded(&events)
            .iter()
            .filter(|e| **e == "provider.reset_credits")
            .count(),
        1
    );
}

#[tokio::test]
async fn disabled_automation_only_blocks_manual_consume_until_pending_credit_expires() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    enable_capability(&provider);
    let store = FakeAccountStore::new("openai", events);
    let mut record = plan(Utc::now() + TimeDelta::minutes(5));
    record.enabled = false;
    record.state.pending = Some(PendingResetCredit {
        credit: record.state.credits[0].clone(),
        redeem_request_id: uuid::Uuid::new_v4().to_string(),
    });
    store
        .automations
        .lock()
        .unwrap()
        .insert("acct_test".into(), record.clone());
    let services = accounts_service(provider.clone(), store.clone()).await;
    let command = ConsumeProviderResetCredit {
        account_id: ProviderAccountId::new("acct_test").unwrap(),
        credit_id: Some("another_card".into()),
        redeem_request_id: uuid::Uuid::new_v4(),
    };
    services
        .accounts()
        .consume_reset_credit(&context("manual-with-pending"), command.clone())
        .await
        .expect_err("unresolved automation is still active");
    assert!(provider.reset_credit_commands().is_empty());
    record.state.pending.as_mut().unwrap().credit.expires_at = Utc::now() - TimeDelta::seconds(1);
    store
        .automations
        .lock()
        .unwrap()
        .insert("acct_test".into(), record);
    services
        .accounts()
        .consume_reset_credit(&context("manual-with-pending"), command)
        .await
        .unwrap();
    assert_eq!(provider.reset_credit_commands().len(), 1);
}
