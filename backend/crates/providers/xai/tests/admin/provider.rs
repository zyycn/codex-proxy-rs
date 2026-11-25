//! 验证 ProviderAdmin 端口的授权、账号和管理合同

use super::*;

#[tokio::test]
async fn account_unavailable_clears_real_selector_cooldowns_only_for_deleted_account() {
    let store = MemoryProviderAccountStore::shared();
    let cooldowns = Arc::new(MemoryCooldownPort::default());
    let ports = ProviderStorePorts::new(
        store.clone(),
        Arc::new(TestLeases),
        Arc::new(TestSessionAffinity::default()),
        Arc::new(TestSessionExclusions),
        Arc::new(TestCatalogCache::default()),
        Arc::new(TestArtifactProfiles),
        Arc::new(TestCredentialState),
        cooldowns.clone(),
        Arc::new(TestRuntimePolicy),
        Arc::new(TestOAuthPending::default()),
        Arc::new(RecordingDiagnostics::default()),
    );
    let repository = GrokCredentialRepository::new(store.clone());
    let selector = GrokAccountSessionSelector::new(
        ProviderKind::new("xai").unwrap(),
        repository.clone(),
        MemoryGrokCatalogCache::shared(),
        Arc::new(crate::support::grok_quota_service(
            repository,
            Arc::new(
                ReqwestGrokModelCatalogTransport::new(Arc::new(OfficialGrokEndpointPolicy))
                    .unwrap(),
            ),
        )),
        ports.leases(),
        cooldowns.clone(),
        ports.account_feedback(),
        Arc::new(crate::support::TestSessionAffinity::default()),
    );
    let bundle = provider_xai::initialize(ports).await.unwrap();
    let models = ["grok-4.5", "grok-4.6"].map(|id| UpstreamModelId::new(id).unwrap());
    for suffix in ["deleted-cooldown", "retained-cooldown"] {
        let input = create_input(suffix, &format!("subject-{suffix}"));
        seed_input(&store, &input).await.unwrap();
        let session = SelectedGrokSession::new(
            input.account_id,
            CredentialRevision::new(1).unwrap(),
            input.secret.access_token,
            SecretValue::new(input.account.subject),
            None,
            GrokSessionBinding::new(suffix).unwrap(),
            (),
        )
        .unwrap();
        // cooldown 必须来自生产 selector 对真实反馈类型的处理，不能由 mock 手工伪造
        selector
            .record_failure(
                &session,
                GrokCredentialFailure::RateLimited {
                    retry_after: Some(Duration::from_secs(60)),
                },
            )
            .await;
        for model in &models {
            selector
                .record_failure(
                    &session,
                    GrokCredentialFailure::ModelQuotaExhausted {
                        upstream_model: model.clone(),
                        retry_after: None,
                    },
                )
                .await;
            assert!(
                cooldowns
                    .scoped_cooldown(
                        session.account_id(),
                        &ProviderCooldownScope::upstream_model(model.clone())
                    )
                    .is_some()
            );
        }
        assert!(cooldowns.cooldown(session.account_id()).is_some());
    }
    let deleted = account_id("deleted-cooldown");
    store.delete_account(&deleted).await.unwrap();
    bundle.admin.account_unavailable(&deleted).await;
    assert!(cooldowns.cooldown(&deleted).is_none());
    let retained = account_id("retained-cooldown");
    assert!(cooldowns.cooldown(&retained).is_some());
    for model in models {
        let scope = ProviderCooldownScope::upstream_model(model);
        assert!(cooldowns.scoped_cooldown(&deleted, &scope).is_none());
        assert!(cooldowns.scoped_cooldown(&retained, &scope).is_some());
    }
}

#[tokio::test]
async fn xai_bundle_exposes_core_admin_and_drains_worker_contributions_once() {
    let mut bundle = provider_xai::initialize(provider_ports())
        .await
        .expect("xAI bundle");

    assert_eq!(bundle.core.clone().name(), "xai");
    assert_eq!(bundle.admin.provider_kind().as_str(), "xai");
    let contributions = std::mem::take(&mut bundle.workers);
    assert_eq!(contributions.len(), 3);
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
    assert!(std::mem::take(&mut bundle.workers).is_empty());
}

#[tokio::test]
async fn xai_quota_catalog_worker_treats_empty_account_pool_as_idle() {
    let mut bundle = provider_xai::initialize(provider_ports())
        .await
        .expect("xAI bundle");

    run_quota_catalog_cycle(&mut bundle)
        .await
        .expect("idle quota cycle");
}

#[tokio::test]
async fn xai_quota_catalog_worker_preserves_store_failures() {
    let store = Arc::new(MemoryProviderAccountStore::default());
    store.fail_provider_listing();
    let mut bundle = provider_xai::initialize(provider_ports_with(
        store,
        Arc::new(TestOAuthPending::default()),
    ))
    .await
    .expect("xAI bundle");

    let error = run_quota_catalog_cycle(&mut bundle)
        .await
        .expect_err("Provider account Store failure");
    assert_eq!(error.as_safe_str(), "账号列表读取失败");
}

#[tokio::test]
async fn xai_quota_catalog_worker_retains_both_failure_stages_and_finishes_the_batch() {
    for refresh_due in [true, false] {
        let store = MemoryProviderAccountStore::shared();
        for suffix in ["broken_one", "broken_two"] {
            let mut input = create_input(suffix, "synthetic-subject");
            input.enabled = suffix == "broken_one";
            let mut account = crate::support::prepare_input(&input).unwrap();
            // 模拟历史持久化凭据损坏，额度与目录读取均应保留各自的失败分类
            account.credential = gateway_core::account::PlaintextCredential::new(Map::new());
            let revision = account.account.revision();
            store.create_account(account).await.unwrap();
            if refresh_due {
                store
                    .apply_quota_access(gateway_core::account::QuotaAccessChange {
                        account_id: input.account_id,
                        expected_revision: revision,
                        state: gateway_core::account::QuotaState::exhausted(
                            gateway_core::account::QuotaEvidence::ProviderDenied,
                            SystemTime::UNIX_EPOCH,
                            None,
                        ),
                    })
                    .await
                    .unwrap();
            }
        }
        let mut bundle = provider_xai::initialize(provider_ports_with(
            store,
            Arc::new(TestOAuthPending::default()),
        ))
        .await
        .unwrap();

        let error = run_quota_catalog_cycle(&mut bundle)
            .await
            .expect_err("invalid stored credentials");
        assert_eq!(error.as_safe_str(), "额度或模型目录同步失败");
        let details = error.error_details().expect("synchronization causes");
        let details: Value = serde_json::from_str(details.as_str()).unwrap();
        let cause = details["causes"]["messages"][0].as_str().unwrap();
        assert!(
            cause.contains("catalog error: Grok provider account store is unavailable"),
            "{cause}"
        );
        if refresh_due {
            assert!(cause.contains("quota failures=2"), "{cause}");
            assert!(
                cause
                    .contains("first quota error: xAI quota credential or billing data is invalid"),
                "{cause}"
            );
        } else {
            assert!(cause.contains("quota failures=0"), "{cause}");
            assert!(!cause.contains("first quota error"), "{cause}");
        }
    }
}

#[tokio::test]
async fn xai_admin_provider_validates_known_billing_breakdown() {
    let bundle = provider_xai::initialize(provider_ports())
        .await
        .expect("xAI bundle");
    let admin = bundle.admin.clone();
    let profile = admin.dashboard_wire_profile().expect("wire profile");
    assert_eq!(profile.provider, "xai");
    assert_eq!(profile.product, "Grok Build");
    assert_eq!(profile.version, "1.0.13");
    assert_eq!(profile.user_agent, "grok-shell/1.0.13 (linux; x86_64)");
    let release = profile.release.expect("release status");
    assert_eq!(release.status, DesktopReleaseStatus::Unchecked);
    assert!(release.checked_at.is_none());

    let billing = admin
        .calculated_billing(&ProviderBillingInput {
            upstream_model_id: "grok-4.5".to_owned(),
            service_tier: None,
            input_tokens: Some(100),
            output_tokens: Some(10),
            cached_input_tokens: Some(25),
            cache_write_input_tokens: Some(0),
            total: CurrencyCost {
                currency: "USD".to_owned(),
                amount: "0.0002175".parse().expect("amount"),
            },
        })
        .expect("billing")
        .expect("known pricing");

    assert_eq!(billing.total_amount.amount.as_str(), "0.0002175");
    assert_eq!(billing.input_price_per_million.amount.as_str(), "2");
    assert_eq!(billing.output_price_per_million.amount.as_str(), "6");

    let priority = admin
        .calculated_billing(&ProviderBillingInput {
            upstream_model_id: "grok-4.6".to_owned(),
            service_tier: Some("priority".to_owned()),
            input_tokens: Some(100),
            output_tokens: Some(10),
            cached_input_tokens: Some(20),
            cache_write_input_tokens: Some(0),
            total: CurrencyCost {
                currency: "USD".to_owned(),
                amount: "0.00046".parse().expect("amount"),
            },
        })
        .expect("计费明细")
        .expect("已知的 Priority 价格");
    assert_eq!(priority.service_tier.as_deref(), Some("priority"));
    assert_eq!(priority.standard_amount.amount.as_str(), "0.00023");
    assert_eq!(priority.input_price_per_million.amount.as_str(), "4");
    assert_eq!(priority.multiplier_percent, 200);
}

#[tokio::test]
async fn xai_legacy_billing_should_not_infer_long_context_flag() {
    let bundle = provider_xai::initialize(provider_ports())
        .await
        .expect("xAI bundle");
    let billing = bundle
        .admin
        .calculated_billing(&ProviderBillingInput {
            upstream_model_id: "grok-4.6".to_owned(),
            service_tier: None,
            input_tokens: Some(200_000),
            output_tokens: Some(0),
            cached_input_tokens: Some(0),
            cache_write_input_tokens: Some(0),
            total: CurrencyCost {
                currency: "USD".to_owned(),
                amount: "0.8".parse().expect("stored total"),
            },
        })
        .expect("legacy billing")
        .expect("matching billing breakdown");

    assert_eq!(billing.total_amount.amount.as_str(), "0.8");
    assert_eq!(billing.input_price_per_million.amount.as_str(), "4");
    assert!(!billing.long_context_billing_applied);
}

#[tokio::test]
async fn xai_admin_provider_restores_full_pending_envelope_and_binds_owner() {
    let pending = Arc::new(TestOAuthPending::default());
    let bundle = provider_xai::initialize(provider_ports_with(
        Arc::new(MemoryProviderAccountStore::default()),
        Arc::clone(&pending),
    ))
    .await
    .expect("xAI bundle");
    let owner = MutationContext {
        actor: MutationActor::AdminSession {
            admin_user_id: "admin-owner".to_owned(),
        },
        request_id: "request-start".to_owned(),
    };
    let flow_id = "pending-envelope";
    let owner_ref = admin_session_owner_ref("admin-owner");
    let payload = pending_payload(flow_id, &owner_ref);
    pending.insert(flow_id, owner_ref, payload);

    let wrong_owner = bundle
        .admin
        .complete_authorization(CompleteAuthorization {
            settings: None,
            context: MutationContext {
                actor: MutationActor::AdminSession {
                    admin_user_id: "different-owner".to_owned(),
                },
                request_id: "request-complete".to_owned(),
            },
            flow_id: flow_id.to_owned(),
            callback_url: format!(
                "{}?code=unused&state=wrong-owner",
                provider_xai::OFFICIAL_REDIRECT_URI
            ),
        })
        .await
        .expect_err("wrong owner");
    assert_eq!(wrong_owner.kind(), ProviderAdminErrorKind::NotFound);
    assert_eq!(pending.len(), 1);

    let invalid_callback = bundle
        .admin
        .complete_authorization(CompleteAuthorization {
            settings: None,
            context: owner,
            flow_id: flow_id.to_owned(),
            callback_url: format!(
                "{}?code=unused&state=invalid-state",
                provider_xai::OFFICIAL_REDIRECT_URI
            ),
        })
        .await
        .expect_err("restored pending must validate callback state");
    assert_eq!(invalid_callback.kind(), ProviderAdminErrorKind::Invalid);
    assert_eq!(pending.len(), 1);

    let mismatched_flow = "pending-envelope-mismatch";
    let owner_ref = admin_session_owner_ref("admin-owner");
    pending.insert(
        mismatched_flow,
        owner_ref.clone(),
        pending_payload("different-envelope", &owner_ref),
    );
    let mismatched_envelope = bundle
        .admin
        .complete_authorization(CompleteAuthorization {
            settings: None,
            context: MutationContext {
                actor: MutationActor::AdminSession {
                    admin_user_id: "admin-owner".to_owned(),
                },
                request_id: "request-complete-mismatch".to_owned(),
            },
            flow_id: mismatched_flow.to_owned(),
            callback_url: format!(
                "{}?code=unused&state=invalid-state",
                provider_xai::OFFICIAL_REDIRECT_URI
            ),
        })
        .await
        .expect_err("flow binding mismatch");
    assert_eq!(mismatched_envelope.kind(), ProviderAdminErrorKind::Invalid);
    assert_eq!(pending.len(), 2);
}

#[tokio::test]
async fn xai_admin_provider_projects_cached_quota_models_and_canonical_export() {
    let store = Arc::new(MemoryProviderAccountStore::default());
    let input = create_input("admin_projection", "subject-admin-projection");
    seed_input(&store, &input).await.expect("create account");
    let account = store.account(&input.account_id).expect("stored account");
    let mut record = account_record(&account);
    let proxy =
        gateway_core::account::OutboundProxy::parse("socks5h://user:pass@proxy.example:1080")
            .unwrap();
    record.outbound_proxy = Some(proxy.clone());
    let catalog_cache = Arc::new(TestCatalogCache::default());
    catalog_cache.seed("plan:standard", ["grok-4.5"]);
    let bundle = provider_xai::initialize(provider_ports_with_catalog(
        Arc::clone(&store),
        Arc::new(TestOAuthPending::default()),
        catalog_cache,
    ))
    .await
    .expect("xAI bundle");
    let admin = bundle.admin.clone();

    let operation = admin
        .connection_test_operation(
            &UpstreamModelId::new("grok-4.5").expect("upstream model"),
            "Reply with exactly OK.",
        )
        .await
        .expect("connection test operation");
    let Operation::Generate(request) = operation else {
        panic!("connection test must be a generate operation");
    };
    let encoded = provider_xai::GrokResponsesRequest::encode(
        &request,
        "grok-4.5",
        &ClientApiKeyId::new("admin_connection_test").expect("client key"),
    )
    .expect("official xAI request");
    assert_eq!(
        encoded.body().get("model").and_then(Value::as_str),
        Some("grok-4.5")
    );
    assert_eq!(
        encoded.body().get("stream").and_then(Value::as_bool),
        Some(true)
    );

    let rolling_usage = AccountUsage {
        account_id: account.id().to_string(),
        request_count: 5,
        success_count: 5,
        input_tokens: Some(100),
        output_tokens: Some(20),
        cached_input_tokens: Some(80),
        cache_write_input_tokens: None,
        reasoning_output_tokens: None,
        image_input_tokens: None,
        image_output_tokens: None,
        image_request_count: 0,
        image_request_failed_count: 0,
        total_tokens: Some(120),
        cost_coverage: Default::default(),
        costs: Vec::new(),
        last_used_at: Some(Utc::now()),
        request_buckets: Vec::new(),
        models: Vec::new(),
    };
    let quota = admin
        .quota(ProviderQuotaRequest {
            account_id: account.id().clone(),
            refresh: false,
            rolling_usage: Some(rolling_usage.clone()),
        })
        .await
        .expect("cached quota");
    assert_eq!(quota.windows.len(), 1);
    assert!(quota.observed_at.is_none());
    assert!(quota.plan_type.is_none());
    assert_eq!(quota.windows[0].window_seconds, Some(86_400));
    assert_eq!(quota.windows[0].local_usage.as_ref(), Some(&rolling_usage));
    assert_eq!(
        quota.usage_window().map(|(_, period)| period),
        Some(AccountUsagePeriod::Daily)
    );
    store
        .compare_and_swap_quota(gateway_core::account::QuotaObservation {
            quota_pause: None,
            expected_connection: None,
            plan_type: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: OpaqueProviderData::new(
                serde_json::from_value(serde_json::json!({
                    "subscriptionTier": "Free",
                    "config": {
                        "creditUsagePercent": 25,
                        "onDemandCap": {"val": 500},
                        "onDemandUsed": {"val": 100},
                        "prepaidBalance": {"val": -500}
                    }
                }))
                .expect("quota document"),
            ),
            observed_at: SystemTime::now(),
            state: account.quota(),
        })
        .await
        .expect("store subscription snapshot");
    let quota = admin
        .quota(ProviderQuotaRequest {
            account_id: account.id().clone(),
            refresh: false,
            rolling_usage: None,
        })
        .await
        .expect("cached subscription");
    assert_eq!(quota.plan_type.as_deref(), Some("Free"));
    assert_eq!(quota.windows.len(), 1);
    assert_eq!(quota.windows[0].used_percent, Some(25.0));
    let window_data = quota.windows[0]
        .provider_data
        .as_ref()
        .expect("quota details")
        .expose_to_provider()
        .expose_to_provider();
    assert_eq!(window_data["onDemandCapCents"], 500);
    assert_eq!(window_data["onDemandUsedCents"], 100);
    assert_eq!(window_data["prepaidBalanceCents"], -500);
    let models = admin
        .models(account.id(), false)
        .await
        .expect("cached models");
    assert_eq!(models.models[0].id.as_str(), "grok-4.5");
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
    assert_eq!(exported.account_ids, vec![input.account_id]);
    let document = exported.document.expose_to_provider().expose_to_provider();
    assert_eq!(document.get("version").and_then(Value::as_u64), Some(1));
    assert_eq!(
        document.get("type").and_then(Value::as_str),
        Some("oauth-account-bundle")
    );
    let entries =
        provider_xai::GrokOAuthImportDocument::parse_json(&serde_json::to_vec(document).unwrap())
            .unwrap()
            .into_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].outbound_proxy(), Some(&proxy));
}

#[tokio::test]
async fn xai_admin_provider_rejects_unprepared_mutations_before_store_commit() {
    let store = Arc::new(MemoryProviderAccountStore::default());
    let input = create_input("admin_invalid", "subject-admin-invalid");
    seed_input(&store, &input).await.expect("create account");
    let account = store.account(&input.account_id).expect("stored account");
    let record = account_record(&account);
    let bundle = provider_xai::initialize(provider_ports_with(
        store,
        Arc::new(TestOAuthPending::default()),
    ))
    .await
    .expect("xAI bundle");
    let admin = bundle.admin.clone();

    let import_error = admin
        .prepare_import(PrepareCredentialImport {
            default_outbound_proxy: None,
            document: ProviderDocument::new(OpaqueProviderData::new(Map::new())),
        })
        .await
        .expect_err("invalid import");
    assert_eq!(import_error.kind(), ProviderAdminErrorKind::Invalid);
    let rotation_error = admin
        .prepare_rotation(PrepareCredentialRotation {
            account: record.clone(),
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
async fn client_profile_preview_and_dashboard_use_saved_configuration() {
    use provider_xai::profile::selection::{GrokClientProfileSelection, VersionMode};
    let bundle = provider_xai::initialize(provider_ports()).await.unwrap();
    let admin = bundle.admin.clone();
    let options = admin.client_profile_options().unwrap();
    assert_eq!(
        options.expose_to_provider()["defaults"]["versionMode"],
        "latest"
    );
    let selection = GrokClientProfileSelection {
        version_mode: VersionMode::Fixed,
        client_version: Some("8.7.6".to_owned()),
        target_os: "windows".to_owned(),
        target_arch: "arm64".to_owned(),
        ..Default::default()
    };
    let configuration = selection.document().unwrap();
    let preview = admin.preview_client_profile(&configuration).unwrap();
    assert_eq!(preview.expose_to_provider()["clientVersion"], "8.7.6");
    assert_eq!(preview.expose_to_provider()["versionSource"], "custom");
    assert!(preview.expose_to_provider()["verifiedAt"].is_null());
    let dashboard = admin.configured_wire_profile(&configuration).unwrap();
    assert_eq!(dashboard.version, "8.7.6");
    assert_eq!(dashboard.target.os_type, "windows");
    assert_eq!(dashboard.target.arch, "arm64");
    assert_eq!(
        dashboard.user_agent,
        preview.expose_to_provider()["userAgent"].as_str().unwrap()
    );
    assert!(dashboard.release.is_none());
    assert!(dashboard.verified_at.is_none());
}

#[tokio::test]
async fn usage_quote_reuses_native_priority_prices() {
    use gateway_admin::ports::provider::ProviderUsagePricingInput;
    use gateway_core::metering::Usage;
    let bundle = provider_xai::initialize(provider_ports()).await.unwrap();
    let admin = bundle.admin.clone();
    let input = ProviderUsagePricingInput {
        upstream_model_id: "grok-4.5".into(),
        service_tier: Some("priority".into()),
        usage: Usage {
            input_tokens: Some(100),
            output_tokens: Some(10),
            cached_input_tokens: Some(25),
            cache_write_input_tokens: Some(0),
            image_input_tokens: Some(0),
            image_output_tokens: Some(0),
            ..Default::default()
        },
    };
    let price = admin.evaluate_usage_pricing(&input, None).unwrap().unwrap();
    assert_eq!(price.total_amount().amount().canonical(), "0.000435");
}
