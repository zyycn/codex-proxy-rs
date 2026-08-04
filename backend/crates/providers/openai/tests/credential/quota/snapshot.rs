//! 快照聚合、窗口滚动和调度信号回归。
//!
//! 覆盖 raw JSON 解析、`limit_reached` 快照级聚合与窗口投影。

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::future::join_all;
use gateway_core::engine::credential::{
    OpaqueProviderData, ProviderAccountStore as _, QuotaObservation, QuotaWriteOutcome,
};
use provider_openai::credential::{CodexQuotaWindowKind, parse_codex_quota_usage};
use serde_json::json;

use super::{MemoryAccountStore, create_account, quota_service};

#[test]
fn parser_extracts_dynamic_windows_without_a_fixed_database_shape() {
    let fact = parse_codex_quota_usage(&json!({
        "rate_limit": {
            "allowed": true,
            "primary_window": {"used_percent": 25.2, "reset_at": 1_800_000_100},
            "secondary_window": {"used_percent": 80.4, "reset_at": 1_800_000_200}
        },
        "additional_rate_limits": [{
            "limit_name": "future_dynamic_window",
            "rate_limit": {"primary_window": {"used_percent": 10}}
        }]
    }))
    .expect("valid dynamic quota");

    assert_eq!(fact.remaining_percent(), Some(20));
    assert_eq!(
        fact.resets_at().map(|value| value.timestamp()),
        Some(1_800_000_100)
    );
    assert!(!fact.exhausted());
}

#[test]
fn parser_treats_any_confirmed_provider_limit_as_exhausted() {
    let fact = parse_codex_quota_usage(&json!({
        "rate_limit": {"primary_window": {"used_percent": 10}},
        "additional_rate_limits": [{
            "rate_limit": {"allowed": false, "primary_window": {"used_percent": 100}}
        }]
    }))
    .expect("valid quota");

    assert!(fact.exhausted());
}

#[test]
fn parser_keeps_full_percent_as_display_without_provider_exhaustion_signal() {
    let fact = parse_codex_quota_usage(&json!({
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {"used_percent": 100, "reset_at": 1_900_000_000}
        }
    }))
    .expect("valid full-percent quota");

    // 任一子窗口 used>=100 是快照级 exhaustion 事实，
    // 即使顶层 allowed=true。
    assert_eq!(fact.remaining_percent(), Some(0));
    assert!(fact.exhausted());
}

#[test]
fn parser_treats_confirmed_limit_reached_as_exhausted_even_below_full_percent() {
    let fact = parse_codex_quota_usage(&json!({
        "rate_limit": {
            "allowed": false,
            "limit_reached": true,
            "primary_window": {"used_percent": 98, "reset_at": 1_900_000_000}
        }
    }))
    .expect("valid provider-confirmed quota");

    assert_eq!(fact.remaining_percent(), Some(2));
    assert!(fact.exhausted());
}

#[test]
fn parser_does_not_infer_exhaustion_from_unknown_credit_fields() {
    let fact = parse_codex_quota_usage(&json!({
        "credits": {
            "has_credits": false,
            "balance": 0,
            "overage_limit_reached": false,
            "future_provider_field": {"anything": true}
        }
    }))
    .expect("recognized credits object");

    assert!(!fact.exhausted());
}

#[test]
fn parser_accepts_official_null_additional_rate_limits() {
    let fact = parse_codex_quota_usage(&json!({
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {
                "limit_window_seconds": 18_000,
                "used_percent": 12.5,
                "reset_at": 1_800_000_100
            },
            "secondary_window": {
                "limit_window_seconds": 604_800,
                "used_percent": 30.0,
                "reset_at": 1_800_000_200
            }
        },
        "additional_rate_limits": null,
        "credits": {
            "has_credits": false,
            "balance": null,
            "overage_limit_reached": false,
            "unlimited": false
        },
        "spend_control": {
            "individual_limit": null,
            "reached": false
        }
    }))
    .expect("official null additional quota");

    assert_eq!(fact.remaining_percent(), Some(70));
    assert!(!fact.exhausted());
}

#[test]
fn parser_rejects_wrong_known_field_type_without_echoing_body() {
    let marker = "quota-secret-marker";
    let error = parse_codex_quota_usage(&json!({
        "rate_limit": {"allowed": marker}
    }))
    .expect_err("known field type must be strict");

    assert!(!format!("{error:?} {error}").contains(marker));
}

#[test]
fn parser_rejects_unrecognized_top_level_object() {
    assert!(parse_codex_quota_usage(&json!({"future_only": {"used": 1}})).is_err());
}
#[tokio::test]
async fn concurrent_cold_scheduling_hydration_reads_quota_once() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_hydration").await;
    let account = store.account("acct_hydration").expect("created account");
    let service = quota_service(&store);

    join_all((0..32).map(|_| service.prepare_scheduling(std::slice::from_ref(&account)))).await;

    assert_eq!(store.quota_reads(), 1);
}

#[tokio::test]
async fn persisted_provider_quota_projects_dynamic_windows_without_network_io() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_quota").await;
    let account = store.account("acct_quota").expect("created account");
    let raw = json!({
        "rate_limit": {
            "allowed": true,
            "primary_window": {
                "used_percent": 37,
                "reset_at": 1_900_000_000,
                "limit_window_seconds": 18_000
            },
            "secondary_window": {
                "used_percent": 42,
                "reset_at": 1_900_604_800,
                "limit_window_seconds": 604_800
            }
        },
        "additional_rate_limits": null,
        "spend_control": {
            "reached": false,
            "individual_limit": {
                "used_percent": 12,
                "reset_at": 1_902_592_000
            }
        },
        "provider_specific_root": {"opaque": [1, 2, 3]}
    });
    let outcome = store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                raw.as_object().expect("quota object").clone(),
            )),
            observed_at: Some(SystemTime::now()),
            limit_reached: None,
        })
        .await
        .expect("persist quota");
    assert_eq!(outcome, QuotaWriteOutcome::Updated);

    let snapshot = quota_service(&store)
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");

    assert_eq!(snapshot.windows().len(), 2);
    assert_eq!(
        snapshot.windows()[0].kind(),
        CodexQuotaWindowKind::ShortTerm
    );
    assert_eq!(snapshot.windows()[0].window_seconds(), Some(18_000));
    assert_eq!(snapshot.windows()[1].kind(), CodexQuotaWindowKind::Weekly);
}

#[tokio::test]
async fn roll_expired_windows_is_o1_for_extremely_old_reset() {
    // 过去数年的 1 秒窗口：O(1) 完成，不能线性循环。
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_o1_roll").await;
    let account = store.account("acct_o1_roll").expect("account");
    let old_reset = SystemTime::now() - Duration::from_secs(5 * 365 * 24 * 3600);
    let raw = json!({
        "rate_limit": {
            "primary_window": {
                "used_percent": 100,
                "reset_at": old_reset
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("epoch")
                    .as_secs(),
                "limit_window_seconds": 1
            }
        }
    });
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                raw.as_object().expect("quota object").clone(),
            )),
            observed_at: Some(SystemTime::now()),
            limit_reached: None,
        })
        .await
        .expect("persist quota");
    let service = quota_service(&store);
    let snapshot = service
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");
    let rolled = snapshot.roll_expired_windows(SystemTime::now());
    let window = &rolled.windows()[0];
    assert_eq!(window.used_percent(), Some(0.0));
    assert!(!window.limit_reached());
    // 下一 reset 必须落在 now 之后（O(1) 公式而非线性累加）。
    assert!(
        window
            .reset_at()
            .is_some_and(|reset| SystemTime::from(reset) > SystemTime::now())
    );
}

#[tokio::test]
async fn scheduling_signal_survives_limit_reached_without_percent_or_reset() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_signal_only_limit").await;
    let account = store.account("acct_signal_only_limit").expect("account");
    let raw = json!({
        "rate_limit": {
            "allowed": false,
            "limit_reached": true,
            "primary_window": {"used_percent": 0}
        }
    });
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                raw.as_object().expect("quota object").clone(),
            )),
            observed_at: Some(SystemTime::now()),
            limit_reached: None,
        })
        .await
        .expect("persist quota");
    let service = quota_service(&store);
    let snapshot = service
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");
    assert!(snapshot.fact().exhausted());
    let signals = service.scheduling_signals(&account);
    assert!(
        signals.is_some_and(|signals| signals.limit_reached()),
        "limit_reached without percent/reset must still produce scheduling exclusion"
    );
}

#[tokio::test]
async fn persisted_codex_alias_keeps_the_top_level_rate_limit_canonical() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_canonical_codex_limit").await;
    let account = store
        .account("acct_canonical_codex_limit")
        .expect("created account");
    let raw = json!({
        "rate_limit": {
            "primary_window": {
                "used_percent": 91,
                "reset_at": 1_900_000_000,
                "limit_window_seconds": 2_592_000
            }
        },
        "additional_rate_limits": [{
            "metered_feature": "codex",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 98,
                    "reset_at": 1_900_000_000,
                    "limit_window_seconds": 2_592_000
                }
            }
        }]
    });
    let outcome = store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                raw.as_object().expect("quota object").clone(),
            )),
            observed_at: Some(SystemTime::now()),
            limit_reached: None,
        })
        .await
        .expect("persist quota");
    assert_eq!(outcome, QuotaWriteOutcome::Updated);

    let snapshot = quota_service(&store)
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");
    let monthly = snapshot
        .windows()
        .iter()
        .filter(|window| window.kind() == CodexQuotaWindowKind::Monthly)
        .collect::<Vec<_>>();

    assert_eq!(snapshot.fact().remaining_percent(), Some(9));
    assert_eq!(monthly.len(), 1);
    assert!(
        monthly
            .iter()
            .any(|window| { window.source() == "codex" && window.used_percent() == Some(91.0) })
    );
}

#[tokio::test]
async fn code_review_limit_projects_as_independent_window_with_limit_name() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_code_review").await;
    let account = store.account("acct_code_review").expect("account");
    let raw = json!({
        "rate_limit": {
            "primary_window": {
                "used_percent": 37,
                "reset_at": 1_900_000_000,
                "limit_window_seconds": 18_000
            }
        },
        "code_review_rate_limit": {
            "primary_window": {
                "used_percent": 80,
                "reset_at": 1_900_604_800,
                "limit_window_seconds": 604_800
            }
        },
        "additional_rate_limits": [{
            "limit_name": "code review",
            "metered_feature": "code_review",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 55,
                    "reset_at": 1_900_604_800,
                    "limit_window_seconds": 604_800
                }
            }
        }],
        "spend_control": {
            "reached": false,
            "individual_limit": {
                "used_percent": 12,
                "reset_at": 1_902_592_000
            }
        }
    });
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                raw.as_object().expect("quota object").clone(),
            )),
            observed_at: Some(SystemTime::now()),
            limit_reached: None,
        })
        .await
        .expect("persist quota");

    let snapshot = quota_service(&store)
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");

    // spend_control 不再生成窗口（只作 exhaustion 信号）；顶层和 additional
    // 的 code_review 桶分别保留，避免一个快照覆盖另一个。
    let review = snapshot
        .windows()
        .iter()
        .filter(|window| window.source() == "code_review")
        .collect::<Vec<_>>();
    assert_eq!(review.len(), 2);
    assert!(review.iter().any(|window| {
        window.limit_name() == Some("code_review") && window.used_percent() == Some(80.0)
    }));
    assert!(review.iter().any(|window| {
        window.limit_name() == Some("code review") && window.used_percent() == Some(55.0)
    }));
    assert_ne!(review[0].key(), review[1].key());
    assert_eq!(snapshot.windows().len(), 3);
    assert!(!snapshot.fact().exhausted());
}
