//! 容量熔断触发：高频容量类失败按滑动窗口计数冻结账号，成功调用清空证据

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gateway_core::provider_ports::{
    ProviderCooldownKind, ProviderCooldownPort as _, ProviderFreezePolicy,
};
use provider_openai::credential::CodexCredentialQuotaService;

use super::{MemoryAccountStore, create_account, wire_profile};
use crate::support::{MemoryCooldownPort, StaticFreezePolicy, TestLeaseCoordinator};

fn freeze_policy(threshold: u32) -> ProviderFreezePolicy {
    ProviderFreezePolicy::try_new(true, threshold, 600, 7_200, true, None, true)
        .expect("valid freeze policy")
}

async fn freeze_service(
    store: &Arc<MemoryAccountStore>,
    policy: ProviderFreezePolicy,
) -> (Arc<CodexCredentialQuotaService>, Arc<MemoryCooldownPort>) {
    let cooldowns = Arc::new(MemoryCooldownPort::new());
    let quota = Arc::new(CodexCredentialQuotaService::new(
        store.repository(),
        wire_profile(),
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        provider_openai::OFFICIAL_CODEX_BASE_URL.to_owned(),
        Arc::clone(&cooldowns) as Arc<dyn gateway_core::provider_ports::ProviderCooldownPort>,
        Arc::new(TestLeaseCoordinator::default()),
        StaticFreezePolicy::policy_port(policy),
    ));
    (quota, cooldowns)
}

#[tokio::test]
async fn capacity_failures_below_threshold_do_not_freeze() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_capacity_below").await;
    let account = store
        .account("acct_capacity_below")
        .expect("created account");
    let (quota, cooldowns) = freeze_service(&store, freeze_policy(3)).await;

    for _ in 0..2 {
        quota
            .apply_capacity_failure(&account, SystemTime::now())
            .await;
    }

    assert_eq!(
        cooldowns
            .capacity_evidence(account.id())
            .map(|(count, _)| count),
        Some(2)
    );
    assert!(
        quota
            .cooldown(account.id())
            .await
            .expect("cooldown read")
            .is_none()
    );
}

#[tokio::test]
async fn capacity_failures_reaching_threshold_write_capacity_freeze_cooldown() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_capacity_freeze").await;
    let account = store
        .account("acct_capacity_freeze")
        .expect("created account");
    let (quota, cooldowns) = freeze_service(&store, freeze_policy(3)).await;

    for _ in 0..3 {
        quota
            .apply_capacity_failure(&account, SystemTime::now())
            .await;
    }

    let until = quota
        .cooldown(account.id())
        .await
        .expect("cooldown read")
        .expect("freeze cooldown written");
    let remaining = until
        .until
        .duration_since(SystemTime::now())
        .expect("freeze in the future");
    assert!(
        remaining > Duration::from_secs(7_000) && remaining <= Duration::from_secs(7_200),
        "freeze should last about 2 hours, got {remaining:?}"
    );
    let cooldown = cooldowns.read(account.id()).await.expect("read cooldown");
    assert_eq!(
        cooldown.expect("cooldown").kind(),
        ProviderCooldownKind::CapacityFreezeProbe
    );
}

#[tokio::test]
async fn disabled_freeze_policy_records_nothing() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_capacity_disabled").await;
    let account = store
        .account("acct_capacity_disabled")
        .expect("created account");
    let (quota, cooldowns) = freeze_service(&store, ProviderFreezePolicy::disabled()).await;

    for _ in 0..5 {
        quota
            .apply_capacity_failure(&account, SystemTime::now())
            .await;
    }

    assert!(cooldowns.capacity_evidence(account.id()).is_none());
    assert!(
        quota
            .cooldown(account.id())
            .await
            .expect("cooldown read")
            .is_none()
    );
}

#[tokio::test]
async fn successful_inference_clears_capacity_evidence() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_capacity_success").await;
    let account = store
        .account("acct_capacity_success")
        .expect("created account");
    let (quota, cooldowns) = freeze_service(&store, freeze_policy(100)).await;

    for _ in 0..3 {
        quota
            .apply_capacity_failure(&account, SystemTime::now())
            .await;
    }
    assert!(cooldowns.capacity_evidence(account.id()).is_some());

    quota
        .record_successful_inference(&account, SystemTime::now())
        .await
        .expect("record success");

    assert!(cooldowns.capacity_evidence(account.id()).is_none());
}

/// 阈值与窗口边界使用与迁移 check 约束一致的构造校验
#[test]
fn freeze_policy_rejects_out_of_range_configuration() {
    for (threshold, window, duration) in [(1, 600, 7_200), (12, 59, 7_200), (12, 600, 299)] {
        assert!(
            ProviderFreezePolicy::try_new(true, threshold, window, duration, true, None, true)
                .is_err()
        );
    }
    assert!(
        ProviderFreezePolicy::try_new(true, 12, 600, 7_200, true, Some(" ".to_owned()), true)
            .is_err()
    );
    assert!(ProviderFreezePolicy::try_new(true, 12, 600, 7_200, true, None, true).is_ok());
}

#[tokio::test]
async fn in_flight_success_does_not_release_a_new_capacity_freeze() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_freeze_late_success").await;
    let account = store.account("acct_freeze_late_success").expect("account");
    let (quota, cooldowns) = freeze_service(&store, freeze_policy(2)).await;
    for _ in 0..2 {
        quota
            .apply_capacity_failure(&account, SystemTime::now())
            .await;
    }
    let before = cooldowns.read(account.id()).await.expect("freeze");
    quota
        .record_successful_inference(&account, SystemTime::now())
        .await
        .expect("late success");
    assert_eq!(
        cooldowns.read(account.id()).await.expect("freeze remains"),
        before
    );
    assert_eq!(
        cooldowns
            .capacity_evidence(account.id())
            .map(|(count, _)| count),
        Some(2)
    );
}
