use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeZone as _, Utc};
use futures::future::BoxFuture;
use gateway_admin::model::accounts::AccountRecord;
use gateway_admin::model::observability::{
    CurrencyCost, DesktopReleaseStatus, ProviderBillingInput,
};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwnerBinding, CompleteAuthorization,
    ConsumeProviderResetCredit, PendingAuthorizationMutation, PrepareCredentialImport,
    PrepareCredentialRefresh, PrepareCredentialRotation, ProviderDocument,
    ProviderExportCredentialInput, ProviderQuotaRequest, ProviderQuotaWindowRole,
    QuotaLocalUsageAttribution,
};
use gateway_admin::model::{MutationActor, MutationContext, Revision};
use gateway_admin::ports::provider::ProviderAdminErrorKind;
use gateway_core::engine::credential::{
    CredentialRevision, CredentialState, OpaqueProviderData, ProviderAccount, ProviderAccountId,
    ProviderAccountStore, QuotaAccessChange, QuotaEvidence, QuotaObservation, QuotaState,
};
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload};
use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingClaimOutcome, OAuthPendingConsumeOutcome,
    OAuthPendingFlowPort, OAuthPendingPutOutcome, OAuthPendingReleaseOutcome,
    ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderCatalogCacheKey,
    ProviderCatalogCachePort, ProviderCooldown, ProviderCooldownPort, ProviderCooldownScope,
    ProviderCredentialState, ProviderCredentialStatePort, ProviderRefreshPolicy,
    ProviderRuntimePolicyPort, ProviderScopedCooldown, ProviderStoreError, ProviderStorePorts,
};
use gateway_core::routing::{ProviderKind, UpstreamModelId};
use gateway_core::task::{WorkerContribution, WorkerKind, WorkerRunnable};
use provider_openai::config::{CodexWireProfileConfig, OpenAiConfig};
use provider_openai::credential::{CodexCredentialCodec, ImportCodexOAuthCredential};
use provider_openai::transport::profile::APPCAST_POLL_INTERVAL;
use secrecy::SecretString;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{
    MemoryAccountStore, MemorySessionAffinity, MemorySessionExclusions, TestLeaseCoordinator,
    profile, secret,
};

#[tokio::test]
async fn openai_bundle_exposes_one_core_provider_and_drains_worker_contributions_once() {
    let config = valid_config();
    let mut bundle = provider_openai::initialize(config.config.clone(), provider_ports())
        .await
        .expect("OpenAI bundle");

    assert_eq!(bundle.core_provider().name(), "openai");
    assert_eq!(bundle.admin_provider().provider_kind().as_str(), "openai");
    let contributions = bundle.take_worker_contributions();
    assert_eq!(contributions.len(), 4);
    assert!(
        contributions
            .iter()
            .any(|item| item.kind() == WorkerKind::OAuthRefresh)
    );
    assert!(
        contributions
            .iter()
            .any(|item| item.kind() == WorkerKind::QuotaCatalogHealth)
    );
    let release_worker = contributions
        .iter()
        .find_map(|contribution| match contribution {
            WorkerContribution::Registration(registration)
                if registration.id.owner() == "openai-desktop-release" =>
            {
                Some(registration)
            }
            WorkerContribution::Registration(_) | WorkerContribution::Disabled { .. } => None,
        })
        .expect("Desktop release worker");
    assert_eq!(release_worker.id.kind(), WorkerKind::QuotaCatalogHealth);
    let WorkerRunnable::Scheduled { schedule, .. } = &release_worker.runnable else {
        panic!("Desktop release worker must be scheduled");
    };
    assert_eq!(schedule.interval(), APPCAST_POLL_INTERVAL);
    assert!(contributions.iter().any(|contribution| {
        matches!(
            contribution,
            WorkerContribution::Registration(registration)
                if registration.id.owner() == "openai-model-etag"
                    && matches!(&registration.runnable, WorkerRunnable::Daemon { .. })
        )
    }));
    assert!(bundle.take_worker_contributions().is_empty());
}

#[tokio::test]
async fn openai_admin_provider_exposes_live_wire_profile_and_validated_billing() {
    let config = valid_config();
    let bundle = provider_openai::initialize(config.config.clone(), provider_ports())
        .await
        .expect("OpenAI bundle");
    let admin = bundle.admin_provider();
    let profile = admin.dashboard_wire_profile().expect("wire profile");
    assert_eq!(profile.version, "0.102.0");
    assert_eq!(profile.build, None);
    assert_eq!(profile.target.os_type, "Mac OS");
    assert_eq!(profile.target.os_version, "15.5.0");
    assert_eq!(
        profile.user_agent,
        "Codex Desktop/0.102.0 (Mac OS 15.5.0; arm64) xterm-256color (Codex Desktop; 1.2026.190)"
    );
    assert_eq!(
        profile
            .attributes
            .iter()
            .find(|attribute| attribute.label == "客户端标识")
            .map(|attribute| attribute.value.as_str()),
        Some("Codex Desktop; 1.2026.190")
    );
    assert_eq!(
        profile.release.as_ref().map(|release| release.status),
        Some(DesktopReleaseStatus::Unchecked)
    );
    let billing = admin
        .calculated_billing(&ProviderBillingInput {
            upstream_model_id: "gpt-4o".to_owned(),
            service_tier: None,
            input_tokens: Some(1_000_000),
            output_tokens: Some(0),
            cached_tokens: Some(0),
            cache_write_tokens: Some(0),
            total: CurrencyCost {
                currency: "USD".to_owned(),
                amount: "2.5".parse().expect("amount"),
            },
        })
        .expect("billing")
        .expect("known pricing");
    assert_eq!(billing.total_amount.amount.as_str(), "2.5");
    assert_eq!(billing.input_price_per_million.amount.as_str(), "2.5");

    let fast_billing = admin
        .calculated_billing(&ProviderBillingInput {
            upstream_model_id: "gpt-4o".to_owned(),
            service_tier: Some("priority".to_owned()),
            input_tokens: Some(1_000_000),
            output_tokens: Some(0),
            cached_tokens: Some(0),
            cache_write_tokens: Some(0),
            total: CurrencyCost {
                currency: "USD".to_owned(),
                amount: "4.25".parse().expect("fast amount"),
            },
        })
        .expect("fast billing")
        .expect("known fast pricing");
    assert_eq!(fast_billing.service_tier.as_deref(), Some("priority"));
    assert_eq!(fast_billing.multiplier_percent, 170);
    assert_eq!(fast_billing.standard_amount.amount.as_str(), "2.5");
    assert_eq!(fast_billing.total_amount.amount.as_str(), "4.25");
}

#[tokio::test]
async fn reset_credit_success_with_invalid_body_should_remain_an_unknown_consume_result() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/wham/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .expect(1)
        .mount(&server)
        .await;
    let (bundle, account_id, _config) = reset_credit_admin(&server).await;

    let error = bundle
        .admin_provider()
        .consume_reset_credit(reset_credit_command(account_id))
        .await
        .expect_err("invalid success body must be ambiguous");

    assert_eq!(error.kind(), ProviderAdminErrorKind::BadGateway);
    assert_eq!(
        error.message(),
        Some(
            "OpenAI reset-credit consume result is unknown; refresh the credit list before retrying"
        )
    );
}

#[tokio::test]
async fn reset_credit_explicit_http_rejection_should_preserve_the_raw_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/wham/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(409).set_body_raw(
            r#"{"code":"nothing_to_reset","detail":"window is fresh"}"#,
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let (bundle, account_id, _config) = reset_credit_admin(&server).await;

    let error = bundle
        .admin_provider()
        .consume_reset_credit(reset_credit_command(account_id))
        .await
        .expect_err("explicit upstream rejection");

    assert_eq!(error.kind(), ProviderAdminErrorKind::BadGateway);
    assert_eq!(
        error.message(),
        Some(
            r#"OpenAI reset-credit upstream returned HTTP 409: {"code":"nothing_to_reset","detail":"window is fresh"}"#
        )
    );
    let debug = format!("{error:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("window is fresh"));
}

#[tokio::test]
async fn openai_core_provider_projects_codex_request_observation_without_routing_side_effects() {
    let config = valid_config();
    let bundle = provider_openai::initialize(config.config.clone(), provider_ports())
        .await
        .expect("OpenAI bundle");
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("summarize")),
            ("reasoning".to_owned(), json!({"effort": "high"})),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([(
        "turn_metadata".to_owned(),
        Value::String(r#"{"request_kind":"compaction","subagent_kind":"review"}"#.to_owned()),
    )]));
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));

    let observation = bundle.core_provider().request_observation(&operation);

    assert_eq!(observation.request_kind.as_deref(), Some("compaction"));
    assert_eq!(observation.subagent_kind.as_deref(), Some("review"));
    // Codex 当前只在特定多代理预设组合下给出 reasoning_preset；普通 high 保持空值。
    assert_eq!(observation.reasoning_preset, None);
    assert!(observation.compact);
}

#[tokio::test]
async fn openai_admin_provider_persists_the_full_pending_envelope_and_binds_owner() {
    let pending = Arc::new(TestOAuthPending::default());
    let config = valid_config();
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(
            Arc::new(MemoryAccountStore::default()),
            Arc::clone(&pending),
        ),
    )
    .await
    .expect("OpenAI bundle");
    let start_context = MutationContext {
        actor: MutationActor::AdminSession {
            admin_user_id: "admin-owner".to_owned(),
        },
        request_id: "request-start".to_owned(),
    };
    let started = bundle
        .admin_provider()
        .start_authorization(PendingAuthorizationMutation::new(
            ProviderKind::new("openai").expect("provider"),
            AuthorizationMutationTarget::Create {
                name: "OAuth account".to_owned(),
            },
            AuthorizationOwnerBinding::from_context(&start_context),
        ))
        .await
        .expect("start authorization");
    {
        let values = pending.values.lock().expect("OAuth pending");
        let (_, payload, _, _) = values.values().next().expect("stored pending");
        let mutation = payload
            .expose_to_provider()
            .get("mutation")
            .and_then(Value::as_object)
            .expect("pending mutation");
        assert!(
            payload
                .expose_to_provider()
                .get("reauthorization_credential_revision")
                .is_none()
        );
        assert!(
            payload
                .expose_to_provider()
                .get("installation_id")
                .and_then(Value::as_str)
                .and_then(|value| uuid::Uuid::parse_str(value).ok())
                .is_some_and(|value| value.get_version_num() == 4)
        );
        assert_eq!(
            mutation.get("schema_version").and_then(Value::as_u64),
            Some(3)
        );
        assert!(mutation.get("expected_config_revision").is_none());
        assert!(
            mutation
                .get("target")
                .and_then(Value::as_object)
                .is_some_and(|target| target.get("expected_credential_revision").is_none())
        );
        assert_eq!(
            mutation.get("started_request_id").and_then(Value::as_str),
            Some("request-start")
        );
    }
    let error = bundle
        .admin_provider()
        .complete_authorization(CompleteAuthorization {
            context: MutationContext {
                actor: MutationActor::AdminSession {
                    admin_user_id: "different-owner".to_owned(),
                },
                request_id: "request-complete".to_owned(),
            },
            flow_id: started.flow_id,
            callback_url: "http://localhost:1455/auth/callback?code=unused&state=unused".to_owned(),
        })
        .await
        .expect_err("wrong owner");
    assert_eq!(error.kind(), ProviderAdminErrorKind::NotFound);
    assert_eq!(pending.values.lock().expect("OAuth pending").len(), 1);
}

#[tokio::test]
async fn openai_reauthorization_pending_payload_reuses_the_account_installation_id() {
    let accounts = Arc::new(MemoryAccountStore::default());
    accounts
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_pending_reauth".to_owned(),
            name: "pending reauthorization".to_owned(),
            secret: secret("pending-reauth-access"),
            verified_account: profile("chatgpt-pending-reauth"),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let account_id = ProviderAccountId::new("acct_pending_reauth").expect("account id");
    let existing = accounts
        .load_current_credential(&account_id)
        .await
        .expect("seeded credential");
    let expected_installation_id = CodexCredentialCodec::decode(&existing.credential)
        .expect("decode seeded credential")
        .installation_id;
    let pending = Arc::new(TestOAuthPending::default());
    let config = valid_config();
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(accounts, Arc::clone(&pending)),
    )
    .await
    .expect("OpenAI bundle");
    let context = MutationContext {
        actor: MutationActor::AdminApiKey,
        request_id: "request-pending-reauth".to_owned(),
    };

    bundle
        .admin_provider()
        .start_authorization(PendingAuthorizationMutation::new(
            ProviderKind::new("openai").expect("provider"),
            AuthorizationMutationTarget::Reauthorize { account_id },
            AuthorizationOwnerBinding::from_context(&context),
        ))
        .await
        .expect("start reauthorization");

    let values = pending.values.lock().expect("OAuth pending");
    let (_, payload, _, _) = values.values().next().expect("stored pending");
    let document = payload.expose_to_provider();
    let mutation = document
        .get("mutation")
        .and_then(Value::as_object)
        .expect("pending mutation");
    let target = mutation
        .get("target")
        .and_then(Value::as_object)
        .expect("pending target");
    assert_eq!(
        mutation.get("schema_version").and_then(Value::as_u64),
        Some(3)
    );
    assert_eq!(
        document
            .get("reauthorization_account_id")
            .and_then(Value::as_str),
        Some("acct_pending_reauth")
    );
    assert_eq!(
        document.get("installation_id").and_then(Value::as_str),
        Some(expected_installation_id.as_str())
    );
    assert!(
        document
            .get("reauthorization_credential_revision")
            .is_none()
    );
    assert_eq!(
        target.get("kind").and_then(Value::as_str),
        Some("reauthorize")
    );
    assert_eq!(
        target.get("account_id").and_then(Value::as_str),
        Some("acct_pending_reauth")
    );
    assert!(target.get("expected_credential_revision").is_none());
}

#[tokio::test]
async fn openai_admin_provider_projects_cached_quota_models_and_canonical_export() {
    let store = Arc::new(MemoryAccountStore::default());
    let mut oauth_secret = secret("admin-projection-access");
    oauth_secret.id_token = Some(SecretString::from("header.id-token.signature"));
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_admin_projection".to_owned(),
            name: "admin projection".to_owned(),
            secret: oauth_secret,
            verified_account: profile("chatgpt-admin-projection"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let account = store
        .account("acct_admin_projection")
        .expect("stored account");
    let record = account_record(&account);
    let config = valid_config();
    let catalog_cache = Arc::new(TestCatalogCache::default());
    catalog_cache.seed("plan:pro", ["gpt-5.4"]);
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with_catalog(
            Arc::clone(&store),
            Arc::new(TestOAuthPending::default()),
            catalog_cache,
        ),
    )
    .await
    .expect("OpenAI bundle");
    let admin = bundle.admin_provider();

    let operation = admin
        .connection_test_operation(
            &UpstreamModelId::new("gpt-5.4").expect("upstream model"),
            "Reply with exactly OK.",
        )
        .expect("connection test operation");
    let Operation::Generate(request) = operation else {
        panic!("connection test must be a generate operation");
    };
    let encoded = provider_openai::encode_generate_request(&request, "gpt-5.4")
        .expect("official OpenAI request");
    assert_eq!(
        encoded.body().get("model").and_then(Value::as_str),
        Some("gpt-5.4")
    );
    assert_eq!(
        encoded.body().get("stream").and_then(Value::as_bool),
        Some(true)
    );

    let account_id = account.id().clone();
    let quota = admin
        .quota(ProviderQuotaRequest {
            account_id: account_id.clone(),
            refresh: false,
            rolling_usage: None,
        })
        .await
        .expect("cached quota");
    assert!(quota.windows.is_empty());
    let models = admin
        .models(&account_id, false)
        .await
        .expect("cached models");
    assert_eq!(models.models[0].id.as_str(), "gpt-5.4");
    let loaded = store
        .load_credential(account.id(), account.revision())
        .await
        .expect("loaded credential");
    let exported = admin
        .export_credentials(vec![ProviderExportCredentialInput {
            account: record,
            provider_material: ProviderDocument::new(OpaqueProviderData::new(
                loaded.credential.into_inner(),
            )),
        }])
        .await
        .expect("canonical export");
    assert_eq!(exported.account_ids, vec![account_id]);
    let document = exported.document.expose_to_provider().expose_to_provider();
    assert_eq!(
        document.get("sourceFormat").and_then(Value::as_str),
        Some("cpr")
    );
    let exported_account = document
        .get("accounts")
        .and_then(Value::as_array)
        .and_then(|accounts| accounts.first())
        .expect("exported OAuth account");
    assert_eq!(
        exported_account.get("accessToken").and_then(Value::as_str),
        Some("admin-projection-access")
    );
    assert_eq!(
        exported_account.get("idToken").and_then(Value::as_str),
        Some("header.id-token.signature")
    );
    assert!(exported_account.get("token").is_none());
}

#[tokio::test]
async fn openai_admin_provider_projects_official_codex_quota_and_independent_buckets_with_chinese_labels()
 {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_admin_canonical_quota".to_owned(),
            name: "admin canonical quota".to_owned(),
            secret: secret("admin-canonical-quota-access"),
            verified_account: profile("chatgpt-admin-canonical-quota"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let account = store
        .account("acct_admin_canonical_quota")
        .expect("stored account");
    let raw = json!({
        "active_limit": "premium",
        "rate_limit": {
            "primary_window": {
                "used_percent": 91,
                "reset_at": 1_900_000_000,
                "limit_window_seconds": 2_592_000
            },
            "secondary_window": {
                "used_percent": 88
            }
        },
        "additional_rate_limits": [{
            "limit_name": "custom_codex_label",
            "metered_feature": "codex",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 2,
                    "reset_at": 1_900_000_000,
                    "limit_window_seconds": 2_592_000
                },
                "secondary_window": {
                    "used_percent": 0
                }
            }
        }, {
            "limit_name": "code_review",
            "metered_feature": "code_review",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 12,
                    "reset_at": 1_900_000_000,
                    "limit_window_seconds": 604_800
                }
            }
        }, {
            "limit_name": "GPT-5.3-Codex-Spark",
            "metered_feature": "codex_bengalfox",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 0,
                    "reset_at": 1_900_000_000,
                    "limit_window_seconds": 604_800
                }
            }
        }]
    });
    let observed_at = SystemTime::now();
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: OpaqueProviderData::new(raw.as_object().expect("quota object").clone()),
            observed_at,
            state: QuotaState::observed_unknown(observed_at),
        })
        .await
        .expect("persist quota");
    let config = valid_config();
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(Arc::clone(&store), Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("OpenAI bundle");

    let quota = bundle
        .admin_provider()
        .quota(ProviderQuotaRequest {
            account_id: account.id().clone(),
            refresh: false,
            rolling_usage: None,
        })
        .await
        .expect("cached quota");
    let monthly = quota
        .windows
        .iter()
        .filter(|window| window.group == "monthly")
        .collect::<Vec<_>>();

    assert_eq!(monthly.len(), 1);
    assert!(monthly.iter().any(|window| {
        window.label == "月额度"
            && window.limit_id.as_deref() == Some("codex")
            && window.used_percent == Some(91.0)
    }));
    assert!(
        !quota
            .windows
            .iter()
            .any(|window| window.key.starts_with("additional-0-codex")),
        "the additional codex alias should not become a second display bucket"
    );
    let secondary = quota
        .windows
        .iter()
        .find(|window| window.key == "codex-secondary")
        .expect("core secondary quota");
    assert_eq!(secondary.label, "次级额度");
    assert_eq!(secondary.used_percent, Some(88.0));
    let review = quota
        .windows
        .iter()
        .find(|window| window.limit_id.as_deref() == Some("code_review"))
        .expect("code review quota");
    assert_eq!(review.label, "周额度");
    assert_eq!(review.limit_name.as_deref(), Some("code_review"));
    assert_eq!(review.role, Some(ProviderQuotaWindowRole::Primary));
    let spark = quota
        .windows
        .iter()
        .find(|window| window.limit_id.as_deref() == Some("codex_bengalfox"))
        .expect("Spark quota");
    assert_eq!(
        (
            monthly[0].local_usage_attribution,
            review.local_usage_attribution,
            spark.local_usage_attribution,
        ),
        (
            QuotaLocalUsageAttribution::AccountWide,
            QuotaLocalUsageAttribution::Unavailable,
            QuotaLocalUsageAttribution::Unavailable,
        ),
    );
}

#[tokio::test]
async fn openai_admin_keeps_confirmed_exhaustion_separate_from_raw_usage_display() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_admin_confirmed_exhaustion";
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_owned(),
            name: "admin confirmed exhaustion".to_owned(),
            secret: secret("admin-confirmed-exhaustion-access"),
            verified_account: profile("chatgpt-admin-confirmed-exhaustion"),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let account = store.account(account_id).expect("stored account");
    let reset_at = 1_900_000_000_u64;
    let observed_at = SystemTime::now();
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: OpaqueProviderData::new(
                json!({
                    "rate_limit": {
                        "allowed": true,
                        "limit_reached": false,
                        "primary_window": {"used_percent": 86, "reset_at": reset_at}
                    }
                })
                .as_object()
                .expect("quota object")
                .clone(),
            ),
            observed_at,
            state: QuotaState::allowed(observed_at),
        })
        .await
        .expect("persist raw quota");
    store
        .apply_quota_access(QuotaAccessChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            state: QuotaState::exhausted(QuotaEvidence::UsageLimitReached, SystemTime::now(), None),
        })
        .await
        .expect("mark account exhausted");
    let config = valid_config();
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(Arc::clone(&store), Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("OpenAI bundle");

    let exhausted = bundle
        .admin_provider()
        .quota(ProviderQuotaRequest {
            account_id: account.id().clone(),
            refresh: false,
            rolling_usage: None,
        })
        .await
        .expect("project exhausted quota");

    assert_eq!(exhausted.windows.len(), 1);
    assert_eq!(exhausted.windows[0].used_percent, Some(86.0));
    let raw = store
        .get_quotas(std::slice::from_ref(account.id()))
        .await
        .expect("read raw quota")
        .pop()
        .expect("raw quota");
    assert_eq!(
        raw.quota.expose_to_provider()["rate_limit"]["primary_window"]["used_percent"],
        86
    );

    store
        .apply_quota_access(QuotaAccessChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            state: QuotaState::allowed(SystemTime::now()),
        })
        .await
        .expect("recover account");
    let recovered = bundle
        .admin_provider()
        .quota(ProviderQuotaRequest {
            account_id: account.id().clone(),
            refresh: false,
            rolling_usage: None,
        })
        .await
        .expect("project recovered quota");

    assert_eq!(recovered.windows[0].used_percent, Some(86.0));
}

#[tokio::test]
async fn openai_admin_provider_rejects_unprepared_mutations_before_store_commit() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_admin_invalid".to_owned(),
            name: "admin invalid".to_owned(),
            secret: secret("admin-invalid-access"),
            verified_account: profile("chatgpt-admin-invalid"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let account = store.account("acct_admin_invalid").expect("stored account");
    let record = account_record(&account);
    let config = valid_config();
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("OpenAI bundle");
    let admin = bundle.admin_provider();
    let import_error = admin
        .prepare_import(PrepareCredentialImport {
            document: ProviderDocument::new(OpaqueProviderData::new(Map::new())),
        })
        .await
        .expect_err("invalid import");
    assert_eq!(import_error.kind(), ProviderAdminErrorKind::Invalid);
    let mut stale_record = record.clone();
    stale_record.name = "stale name".to_owned();
    stale_record.email = None;
    stale_record.plan_type = None;
    stale_record.credential_revision = Revision::new(99).expect("stale revision");
    stale_record.has_refresh_token = false;
    stale_record.access_token_expires_at = None;
    stale_record.next_refresh_at = None;
    stale_record.enabled = false;
    stale_record.credential_state = CredentialState::Banned;
    let rotation_error = admin
        .prepare_rotation(PrepareCredentialRotation {
            account: stale_record,
            provider_material: ProviderDocument::new(OpaqueProviderData::new(Map::new())),
        })
        .await
        .expect_err("invalid rotation");
    assert_eq!(rotation_error.kind(), ProviderAdminErrorKind::Invalid);
    let mut missing = record;
    missing.id = "acct_admin_missing".to_owned();
    let refresh_error = admin
        .prepare_refresh(PrepareCredentialRefresh { account: missing })
        .await
        .expect_err("missing refresh target");
    assert_eq!(refresh_error.kind(), ProviderAdminErrorKind::NotFound);
}

#[tokio::test]
async fn openai_rotation_preserves_the_new_access_token_jwt_expiration() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_admin_rotation_expiration".to_owned(),
            name: "admin rotation expiration".to_owned(),
            secret: secret("admin-rotation-access"),
            verified_account: profile("chatgpt-admin-rotation-expiration"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account = store
        .account("acct_admin_rotation_expiration")
        .expect("stored account");
    let record = account_record(&account);
    let config = valid_config();
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("OpenAI bundle");

    let expires_at = Utc
        .timestamp_opt(2_000_000_000, 0)
        .single()
        .expect("valid test timestamp");
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({"exp": expires_at.timestamp()}))
            .expect("test JWT payload"),
    );
    let mut material = Map::new();
    material.insert(
        "access_token".to_owned(),
        Value::String(format!("unverified-header.{payload}.unverified-signature")),
    );
    material.insert(
        "refresh_token".to_owned(),
        Value::String("admin-rotation-refresh".to_owned()),
    );

    let prepared = bundle
        .admin_provider()
        .prepare_rotation(PrepareCredentialRotation {
            account: record,
            provider_material: ProviderDocument::new(OpaqueProviderData::new(material)),
        })
        .await
        .expect("JWT rotation should be prepared");

    assert_eq!(prepared.facts().access_token_expires_at, Some(expires_at));
}

async fn reset_credit_admin(
    server: &MockServer,
) -> (
    provider_openai::ProviderBundle,
    ProviderAccountId,
    TestOpenAiConfig,
) {
    let account_id = ProviderAccountId::new("acct_reset_credit").expect("account ID");
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_string(),
            name: "reset credit".to_owned(),
            secret: secret("reset-credit-access"),
            verified_account: profile("chatgpt-reset-credit"),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let mut config = valid_config();
    config.config.api.base_url = server.uri();
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("OpenAI reset-credit bundle");
    (bundle, account_id, config)
}

fn reset_credit_command(account_id: ProviderAccountId) -> ConsumeProviderResetCredit {
    ConsumeProviderResetCredit {
        account_id,
        credit_id: Some("credit_1".to_owned()),
        redeem_request_id: Uuid::parse_str("8fbf302d-11df-4bd5-82e4-08e4b3df7874")
            .expect("UUID v4"),
    }
}

fn provider_ports() -> ProviderStorePorts {
    provider_ports_with(
        Arc::new(MemoryAccountStore::default()),
        Arc::new(TestOAuthPending::default()),
    )
}

fn provider_ports_with(
    accounts: Arc<MemoryAccountStore>,
    pending: Arc<TestOAuthPending>,
) -> ProviderStorePorts {
    provider_ports_with_catalog(accounts, pending, Arc::new(TestCatalogCache::default()))
}

fn provider_ports_with_catalog(
    accounts: Arc<MemoryAccountStore>,
    pending: Arc<TestOAuthPending>,
    catalog_cache: Arc<TestCatalogCache>,
) -> ProviderStorePorts {
    ProviderStorePorts::new(
        accounts,
        Arc::new(TestLeaseCoordinator::default()),
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(MemorySessionExclusions::default()),
        catalog_cache,
        Arc::new(TestArtifactProfiles),
        Arc::new(TestCredentialState),
        Arc::new(TestCooldown),
        Arc::new(TestRuntimePolicy),
        pending,
    )
}

fn account_record(account: &ProviderAccount) -> AccountRecord {
    let now = Utc::now();
    AccountRecord {
        id: account.id().to_string(),
        provider_kind: account.provider().clone(),
        groups: Vec::new(),
        name: account.name().to_owned(),
        email: account.email().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().map(str::to_owned),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        plan_type: account.plan_type().map(str::to_owned),
        authentication_kind: account.authentication_kind().to_owned(),
        credential_revision: Revision::new(account.revision().get()).expect("revision"),
        has_refresh_token: account.has_refresh_token(),
        access_token_expires_at: account.access_token_expires_at().map(DateTime::<Utc>::from),
        next_refresh_at: account.next_refresh_at().map(DateTime::<Utc>::from),
        enabled: account.enabled(),
        concurrency_limit: account.concurrency_limit(),
        weight: account.weight(),
        credential_state: account.credential_state(),
        credential_observed_at: now,
        quota: account.quota(),
        last_error_reason: account.last_error_reason(),
        last_error_message: None,
        created_at: now,
        updated_at: now,
    }
}

struct TestOpenAiConfig {
    config: OpenAiConfig,
    _runtime: TempDir,
}

fn valid_config() -> TestOpenAiConfig {
    let mut config = OpenAiConfig::default();
    config.wire_profile = CodexWireProfileConfig {
        originator: "Codex Desktop".to_owned(),
        codex_version: "0.102.0".to_owned(),
        desktop_version: "1.2026.190".to_owned(),
        desktop_build: "19012345678".to_owned(),
        os_type: "Mac OS".to_owned(),
        os_version: "15.5.0".to_owned(),
        arch: "arm64".to_owned(),
        terminal: "xterm-256color".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 19, 0, 0, 0)
            .single()
            .expect("valid test time"),
    };
    let runtime = tempfile::tempdir().expect("test runtime directory");
    config
        .resolve_and_validate(&runtime.path().join("deploy"))
        .expect("valid OpenAI test configuration");
    TestOpenAiConfig {
        config,
        _runtime: runtime,
    }
}

struct TestArtifactProfiles;

impl ProviderArtifactProfileCachePort for TestArtifactProfiles {
    fn replace_if_newer(
        &self,
        _profile: ProviderArtifactProfile,
        _ttl: Duration,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(true) })
    }

    fn read<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<Option<ProviderArtifactProfile>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Default)]
struct TestCatalogCache {
    values: Mutex<BTreeMap<String, OpaqueProviderData>>,
}

impl ProviderCatalogCachePort for TestCatalogCache {
    fn replace<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
        catalog: &'a OpaqueProviderData,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            self.values
                .lock()
                .expect("catalog cache")
                .insert(key.scope().as_str().to_owned(), catalog.clone());
            Ok(())
        })
    }

    fn read<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
    ) -> BoxFuture<'a, Result<Option<OpaqueProviderData>, ProviderStoreError>> {
        Box::pin(async move {
            Ok(self
                .values
                .lock()
                .expect("catalog cache")
                .get(key.scope().as_str())
                .cloned())
        })
    }
}

impl TestCatalogCache {
    fn seed(&self, scope: &str, models: impl IntoIterator<Item = &'static str>) {
        let mut document = Map::new();
        document.insert("version".to_owned(), Value::from(1));
        document.insert("scope".to_owned(), Value::String(scope.to_owned()));
        document.insert(
            "observedAt".to_owned(),
            Value::String(Utc::now().to_rfc3339()),
        );
        document.insert(
            "models".to_owned(),
            Value::Array(
                models
                    .into_iter()
                    .map(|model| Value::String(model.to_owned()))
                    .collect(),
            ),
        );
        self.values
            .lock()
            .expect("catalog cache")
            .insert(scope.to_owned(), OpaqueProviderData::new(document));
    }
}

struct TestCredentialState;

impl ProviderCredentialStatePort for TestCredentialState {
    fn replace(
        &self,
        _state: ProviderCredentialState,
    ) -> BoxFuture<'_, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn read<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCredentialState>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn record_refresh_backoff<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _window: Duration,
    ) -> BoxFuture<'a, Result<u32, ProviderStoreError>> {
        Box::pin(async { Ok(1) })
    }

    fn clear_refresh_backoff<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

struct TestCooldown;

impl ProviderCooldownPort for TestCooldown {
    fn put_if_later(
        &self,
        _cooldown: ProviderCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn put_scoped_if_later(
        &self,
        _cooldown: ProviderScopedCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn clear_all<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
}

struct TestRuntimePolicy;

impl ProviderRuntimePolicyPort for TestRuntimePolicy {
    fn load_refresh_policy(
        &self,
    ) -> BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>> {
        Box::pin(async {
            ProviderRefreshPolicy::try_new(
                Duration::from_secs(300),
                NonZeroU32::new(4).expect("nonzero concurrency"),
            )
        })
    }
}

#[derive(Default)]
struct TestOAuthPending {
    values: Mutex<BTreeMap<PendingKey, PendingValue>>,
}

type PendingKey = (String, String);
type PendingValue = (String, OpaqueProviderData, SystemTime, Option<String>);

impl OAuthPendingFlowPort for TestOAuthPending {
    fn put_if_absent(
        &self,
        flow: NewOAuthPendingFlow,
    ) -> BoxFuture<'_, Result<OAuthPendingPutOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                flow.provider_kind().as_str().to_owned(),
                flow.flow().expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            if values.contains_key(&key) {
                return Ok(OAuthPendingPutOutcome::AlreadyExists);
            }
            values.insert(
                key,
                (
                    flow.owner().expose_to_store().to_owned(),
                    flow.payload().clone(),
                    SystemTime::now() + flow.ttl(),
                    None,
                ),
            );
            Ok(OAuthPendingPutOutcome::Stored)
        })
    }

    fn claim_if_owner<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a gateway_core::provider_ports::OAuthPendingBinding,
        owner: &'a gateway_core::provider_ports::OAuthPendingBinding,
        claim: &'a gateway_core::provider_ports::OAuthPendingBinding,
        _claim_ttl: Duration,
    ) -> BoxFuture<'a, Result<OAuthPendingClaimOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                flow.expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            let Some((stored_owner, payload, expires_at, stored_claim)) = values.get_mut(&key)
            else {
                return Ok(OAuthPendingClaimOutcome::NotFound);
            };
            if *expires_at <= SystemTime::now() {
                values.remove(&key);
                return Ok(OAuthPendingClaimOutcome::NotFound);
            }
            if stored_owner != owner.expose_to_store() {
                return Ok(OAuthPendingClaimOutcome::OwnerMismatch);
            }
            if stored_claim.is_some() {
                return Ok(OAuthPendingClaimOutcome::InProgress);
            }
            *stored_claim = Some(claim.expose_to_store().to_owned());
            Ok(OAuthPendingClaimOutcome::Claimed(payload.clone()))
        })
    }

    fn release_claim<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a gateway_core::provider_ports::OAuthPendingBinding,
        owner: &'a gateway_core::provider_ports::OAuthPendingBinding,
        claim: &'a gateway_core::provider_ports::OAuthPendingBinding,
    ) -> BoxFuture<'a, Result<OAuthPendingReleaseOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                flow.expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            let Some((stored_owner, _, _, stored_claim)) = values.get_mut(&key) else {
                return Ok(OAuthPendingReleaseOutcome::NotFound);
            };
            if stored_owner != owner.expose_to_store() {
                return Ok(OAuthPendingReleaseOutcome::OwnerMismatch);
            }
            if stored_claim.as_deref() != Some(claim.expose_to_store()) {
                return Ok(OAuthPendingReleaseOutcome::ClaimMismatch);
            }
            *stored_claim = None;
            Ok(OAuthPendingReleaseOutcome::Released)
        })
    }

    fn consume_claim<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a gateway_core::provider_ports::OAuthPendingBinding,
        owner: &'a gateway_core::provider_ports::OAuthPendingBinding,
        claim: &'a gateway_core::provider_ports::OAuthPendingBinding,
    ) -> BoxFuture<'a, Result<OAuthPendingConsumeOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                flow.expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            let Some((stored_owner, _, _, stored_claim)) = values.get(&key) else {
                return Ok(OAuthPendingConsumeOutcome::NotFound);
            };
            if stored_owner != owner.expose_to_store() {
                return Ok(OAuthPendingConsumeOutcome::OwnerMismatch);
            }
            if stored_claim.as_deref() != Some(claim.expose_to_store()) {
                return Ok(OAuthPendingConsumeOutcome::ClaimMismatch);
            }
            values.remove(&key);
            Ok(OAuthPendingConsumeOutcome::Consumed)
        })
    }
}
