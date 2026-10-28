//! 凭据提交必须在返回前使旧保护反馈失效，并保留账号与模型限流

use super::*;
use gateway_admin::ports::store::AccountRuntimeStore as _;
use gateway_core::{
    account::{AccountErrorProtection, AccountProtectionMatch},
    provider_ports::{
        ProviderCooldownKind, ProviderCooldownPort, ProviderCooldownScope, ProviderScopedCooldown,
    },
    routing::{ConfigRevision, UpstreamModelId},
};
use gateway_store::redis::{
    CredentialCooldown, CredentialCooldownRepository, RedisAdminAccountRuntimeStore,
    RedisCredentialCooldownRepository, RedisCredentialLeaseRepository,
};
use std::sync::Arc;

#[tokio::test]
async fn credential_commits_fence_protection_without_clearing_rate_limits() {
    let Some(url) = crate::support::test_env("CPR_TEST_REDIS_URL") else {
        return;
    };
    let Some(database) = TestDatabase::create("protection_credential_commit").await else {
        return;
    };
    let mut connection = redis::Client::open(url)
        .unwrap()
        .get_connection_manager()
        .await
        .unwrap();
    let namespace = format!("protection-credential-test-{}", uuid::Uuid::new_v4());
    let cooldowns =
        Arc::new(RedisCredentialCooldownRepository::new(connection.clone(), &namespace).unwrap());
    let runtime = RedisAdminAccountRuntimeStore::new(
        cooldowns.as_ref().clone(),
        RedisCredentialLeaseRepository::new(connection.clone(), &namespace).unwrap(),
    );
    let repository = PgProviderAccountRepository::new(database.pool.clone())
        .with_account_error_protection(cooldowns.clone());
    let id = ProviderAccountId::new("acct_protection_commit").unwrap();
    repository
        .insert_provider_account(account(id.as_str(), "user-protection-commit"))
        .await
        .unwrap();
    let policy:AccountErrorProtection=serde_json::from_value(json!({"enabled":true,"rules":[{"id":"arbitrary","name":"配置规则","conditions":[{"field":"code","operator":"equals","values":["custom_failure"],"ignoreCase":false}]}],"threshold":1,"windowSeconds":60,"durationSeconds":60,"probeEnabled":true,"probeModel":null})).unwrap();
    let matches = [AccountProtectionMatch {
        id: "arbitrary".into(),
        name: "配置规则".into(),
    }];
    let policy_revision = ConfigRevision::new(1).unwrap();
    cooldowns
        .publish_protection_policy(&policy, policy_revision)
        .await
        .unwrap();
    let model =
        ProviderCooldownScope::upstream_model(UpstreamModelId::new("preserved-model").unwrap());
    for revision in 1..=3 {
        let credential_revision = CredentialRevision::new(revision).unwrap();
        let guard = cooldowns
            .begin_protection(
                &id,
                credential_revision,
                &policy.fingerprint(),
                policy_revision,
            )
            .await
            .unwrap()
            .unwrap();
        let delayed = cooldowns
            .begin_protection(
                &id,
                credential_revision,
                &policy.fingerprint(),
                policy_revision,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            cooldowns
                .record_protection_failure(&guard, &policy, &matches)
                .await
                .unwrap()
        );
        cooldowns
            .cache_credential_cooldown(&CredentialCooldown {
                provider_account_id: id.to_string(),
                credential_revision: Revision::new(revision).unwrap(),
                cooldown_until: Utc::now() + TimeDelta::minutes(5),
                kind: ProviderCooldownKind::RateLimit,
            })
            .await
            .unwrap();
        cooldowns
            .put_scoped_if_later(ProviderScopedCooldown::new(
                id.clone(),
                credential_revision,
                model.clone(),
                SystemTime::now() + Duration::from_secs(300),
            ))
            .await
            .unwrap();
        let before = admin_account_store(&database.pool)
            .load_account(id.as_str(), runtime.active_rate_limits().await.unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(
            before
                .projection
                .cooldown
                .unwrap()
                .kind
                .is_error_protection()
        );
        assert_eq!(before.protection_rules, matches);
        match revision {
            1 => {
                let refresh = CredentialCasUpdate::new(
                    id.clone(),
                    credential_revision,
                    ProviderAccountUpdate {
                        account_id: id.clone(),
                        name: "refreshed".into(),
                        email: None,
                        plan_type: None,
                    },
                    PlaintextCredential::new(
                        json!({"access_token":"new-fixture"})
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                    false,
                    None,
                    None,
                )
                .unwrap();
                assert!(matches!(
                    repository
                        .compare_and_swap_credential(refresh)
                        .await
                        .unwrap(),
                    CredentialCasOutcome::Updated(_)
                ));
            }
            2 => {
                repository
                    .rotate_provider_account(RotateProviderAccount {
                        settings: None,
                        scope: ProviderAccountAdminScope {
                            provider_kind: "openai".into(),
                        },
                        profile: profile(id.as_str(), "rotated"),
                        replacement_identity: None,
                        credential: credential_update(id.as_str(), revision, "rotated-fixture"),
                        audit: audit("audit_protection_rotate", "rotate", id.as_str()),
                    })
                    .await
                    .unwrap();
            }
            _ => {
                repository
                    .compare_and_swap_credentials(credential_update(
                        id.as_str(),
                        revision,
                        "cas-fixture",
                    ))
                    .await
                    .unwrap();
            }
        }
        assert!(
            !cooldowns
                .record_protection_failure(&delayed, &policy, &matches)
                .await
                .unwrap()
        );
        assert!(
            !cooldowns
                .advance_protection_revision(&id, credential_revision)
                .await
                .unwrap(),
            "迟到提交通知不能回退版本"
        );
        assert_eq!(
            cooldowns.read(&id).await.unwrap().unwrap().kind(),
            ProviderCooldownKind::RateLimit
        );
        assert!(cooldowns.read_scoped(&id, &model).await.unwrap().is_some());
        let after = admin_account_store(&database.pool)
            .load_account(id.as_str(), runtime.active_rate_limits().await.unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after.projection.cooldown.unwrap().kind,
            ProviderCooldownKind::RateLimit
        );
        assert!(after.protection_rules.is_empty());
    }
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    if !keys.is_empty() {
        let _: u64 = redis::cmd("DEL")
            .arg(keys)
            .query_async(&mut connection)
            .await
            .unwrap();
    }
    database.close().await;
}
