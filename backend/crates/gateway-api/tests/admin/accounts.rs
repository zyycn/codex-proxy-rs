mod query {
    use gateway_admin::model::accounts::{AccountSortField, AccountStatus, SortDirection};
    use gateway_api::admin::accounts::ListQuery;
    use serde_json::json;

    #[test]
    fn account_query_should_parse_provider_status_and_sort_once() {
        let query: ListQuery = serde_json::from_value(json!({
            "page": 3,
            "pageSize": 20,
            "provider": "xai",
            "search": "  operator  ",
            "status": "normal",
            "sortBy": "lastUsedAt",
            "sortDirection": "desc"
        }))
        .expect("deserialize account query");
        let query = query.validate().expect("validate account query");
        assert_eq!(query.page, 3);
        assert_eq!(query.page_size.get(), 20);
        assert!(matches!(
            query.provider_kind,
            Some(ref provider) if provider.as_str() == "xai"
        ));
        assert_eq!(query.search.as_deref(), Some("operator"));
        assert_eq!(query.status, Some(AccountStatus::Normal));
        assert_eq!(
            query.sort.expect("sort").field,
            AccountSortField::LastUsedAt
        );
        assert_eq!(
            query.sort.expect("copy sort").direction,
            SortDirection::Desc
        );
    }

    #[test]
    fn account_query_should_parse_rate_limited_status() {
        let query: ListQuery = serde_json::from_value(json!({
            "status": "rate_limited"
        }))
        .expect("deserialize account query");

        assert_eq!(
            query.validate().expect("validate account query").status,
            Some(AccountStatus::RateLimited)
        );
    }

    #[test]
    fn account_query_should_reject_unbounded_page_size() {
        let query: ListQuery =
            serde_json::from_value(json!({"pageSize": 201})).expect("deserialize account query");
        assert_eq!(
            query.validate().expect_err("reject page size").field(),
            "pageSize"
        );
    }

    #[test]
    fn account_query_should_reject_incomplete_sort() {
        let query: ListQuery =
            serde_json::from_value(json!({"sortBy": "usage"})).expect("deserialize account query");
        assert_eq!(query.validate().expect_err("reject sort").field(), "sort");
    }

    #[test]
    fn account_query_should_reject_unknown_fields() {
        assert!(serde_json::from_value::<ListQuery>(json!({"id": "cred_1"})).is_err());
    }
}

mod usage_statistics {
    use std::str::FromStr as _;

    use chrono::{NaiveDate, TimeZone as _, Utc};
    use gateway_admin::model::{
        observability::{CurrencyCost, DecimalAmount},
        provider_credentials::{
            ProviderUsageStatistics, ProviderUsageStatisticsCycle, ProviderUsageStatisticsDay,
            ProviderUsageStatisticsMode, ProviderUsageStatisticsModel,
            ProviderUsageStatisticsServiceTier, ProviderUsageStatisticsSummary,
            ProviderUsageStatisticsTokens,
        },
    };
    use gateway_api::admin::accounts::{AccountUsageStatisticsData, AccountUsageStatisticsQuery};
    use serde_json::json;

    fn tokens(total: u64) -> ProviderUsageStatisticsTokens {
        ProviderUsageStatisticsTokens {
            uncached_input: total / 4,
            cached_input: total / 2,
            output: total / 4,
            total,
        }
    }

    fn usd(amount: &str) -> CurrencyCost {
        CurrencyCost {
            currency: "USD".to_owned(),
            amount: DecimalAmount::from_str(amount).expect("USD amount"),
        }
    }

    #[test]
    fn usage_statistics_query_accepts_only_current_or_bounded_history_offsets() {
        let query: AccountUsageStatisticsQuery = serde_json::from_value(json!({
            "accountId": "acct_openai",
            "cycleOffset": -3,
            "utcOffsetMinutes": 480
        }))
        .expect("decode usage-statistics query");
        let request = query.into_request().expect("valid cycle offset");
        assert_eq!(request.cycle_offset, -3);
        assert_eq!(request.utc_offset_minutes, 480);

        for (cycle_offset, field) in [(-9, "cycleOffset"), (1, "cycleOffset")] {
            let query: AccountUsageStatisticsQuery = serde_json::from_value(json!({
                "accountId": "acct_openai",
                "cycleOffset": cycle_offset
            }))
            .expect("decode bounded integer");
            assert_eq!(
                query
                    .into_request()
                    .expect_err("reject cycle offset")
                    .field(),
                field
            );
        }
        assert!(
            serde_json::from_value::<AccountUsageStatisticsQuery>(json!({
                "accountId": "acct_openai",
                "historyCycles": 8
            }))
            .is_err()
        );
    }

    #[test]
    fn usage_statistics_response_serializes_calculated_rows_without_raw_documents() {
        let start_at = Utc
            .with_ymd_and_hms(2026, 8, 24, 7, 50, 0)
            .single()
            .expect("cycle start");
        let end_at = Utc
            .with_ymd_and_hms(2026, 8, 31, 7, 50, 0)
            .single()
            .expect("cycle end");
        let response = AccountUsageStatisticsData::from(ProviderUsageStatistics {
            mode: ProviderUsageStatisticsMode::Personal,
            cycle: ProviderUsageStatisticsCycle {
                offset: 0,
                start_at,
                end_at,
                window_seconds: 604_800,
                used_percent: Some(20.0),
                is_current: true,
                can_go_previous: true,
                can_go_next: false,
            },
            summary: ProviderUsageStatisticsSummary {
                tokens: tokens(100),
                estimated_cost: Some(usd("1.25")),
                projected_tokens: Some(500),
                projected_cost: Some(usd("6.25")),
                day_count: 1,
                has_unknown_pricing: false,
                has_missing_token_data: false,
            },
            models: vec![ProviderUsageStatisticsModel {
                key: "gpt-5.6-sol::standard".to_owned(),
                model: "gpt-5.6-sol".to_owned(),
                service_tier: ProviderUsageStatisticsServiceTier::Standard,
                credit_share: Some(1.0),
                quota_share: Some(0.2),
                tokens: tokens(100),
                estimated_cost: Some(usd("1.25")),
                has_unknown_pricing: false,
                has_estimated_allocation: true,
                has_rate_fallback: false,
                has_missing_token_data: false,
            }],
            daily: vec![ProviderUsageStatisticsDay {
                date: NaiveDate::from_ymd_opt(2026, 8, 25).expect("report date"),
                credit_share: Some(1.0),
                tokens: tokens(100),
                estimated_cost: Some(usd("1.25")),
                has_unknown_pricing: false,
                has_missing_token_data: false,
                is_boundary_day: true,
            }],
        });
        let value = serde_json::to_value(response).expect("serialize usage statistics");

        assert_eq!(value["cycle"]["offset"], 0);
        assert_eq!(
            value["summary"]["estimatedCost"],
            json!({
                "currency": "USD",
                "amount": "1.25"
            })
        );
        assert_eq!(value["models"][0]["serviceTier"], "standard");
        assert!(value["models"][0].get("mode").is_none());
        assert_eq!(value["daily"][0]["tokens"]["total"], 100);
        assert_eq!(value["daily"][0]["isBoundaryDay"], true);
        assert!(value["cycle"].get("deltaPercent").is_none());
        assert!(value.get("modelBreakdown").is_none());
        assert!(value.get("dailyTotals").is_none());
        assert!(value.get("usage").is_none());
    }
}

mod batch_update {
    use gateway_api::admin::accounts::{BatchUpdateAccountsRequest, UpdateAccountRequest};
    use serde_json::json;

    #[test]
    fn batch_update_should_accept_complete_atomic_payload() {
        let request: BatchUpdateAccountsRequest = serde_json::from_value(json!({
            "accountIds": ["acct_openai", "acct_xai"],
            "enabled": false,
            "concurrencyLimit": null,
            "weight": 1,
            "groupIds": ["grp_00000000000000000000000000000001"]
        }))
        .expect("deserialize batch update");

        request.validate().expect("validate batch update");
    }

    #[test]
    fn batch_update_should_reject_duplicate_or_invalid_account_ids() {
        for account_ids in [
            json!(["acct_same", "acct_same"]),
            json!(["not-an-account"]),
            json!([]),
        ] {
            let request: BatchUpdateAccountsRequest = serde_json::from_value(json!({
                "accountIds": account_ids,
                "enabled": true,
                "concurrencyLimit": 8,
                "weight": 100,
                "groupIds": []
            }))
            .expect("deserialize invalid batch update");
            assert_eq!(
                request.validate().expect_err("reject account IDs").field(),
                "accountIds"
            );
        }
    }

    #[test]
    fn batch_update_should_require_enabled_and_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<BatchUpdateAccountsRequest>(json!({
            "accountIds": ["acct_test"],
            "concurrencyLimit": null,
            "weight": 1,
            "groupIds": []
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<BatchUpdateAccountsRequest>(json!({
                "accountIds": ["acct_test"],
                "enabled": true,
                "concurrencyLimit": null,
                "weight": 1,
                "groupIds": [],
                "legacy": true
            }))
            .is_err()
        );
    }

    #[test]
    fn batch_update_should_reject_invalid_scheduling_bounds_and_missing_fields() {
        for (concurrency_limit, weight, field) in [
            (json!(0), json!(1), "concurrencyLimit"),
            (json!(4294967296_u64), json!(1), "concurrencyLimit"),
            (json!(null), json!(0), "weight"),
            (json!(null), json!(101), "weight"),
        ] {
            let request: BatchUpdateAccountsRequest = serde_json::from_value(json!({
                "accountIds": ["acct_test"],
                "enabled": true,
                "concurrencyLimit": concurrency_limit,
                "weight": weight,
                "groupIds": []
            }))
            .expect("deserialize invalid scheduling");
            assert_eq!(
                request.validate().expect_err("reject scheduling").field(),
                field
            );
        }
        assert!(
            serde_json::from_value::<BatchUpdateAccountsRequest>(json!({
                "accountIds": ["acct_test"],
                "enabled": true,
                "weight": 1,
                "groupIds": []
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<BatchUpdateAccountsRequest>(json!({
                "accountIds": ["acct_test"],
                "enabled": true,
                "concurrencyLimit": null,
                "groupIds": []
            }))
            .is_err()
        );
    }

    #[test]
    fn single_update_should_require_the_same_complete_scheduling_contract() {
        let request: UpdateAccountRequest = serde_json::from_value(json!({
            "accountId": "acct_test",
            "enabled": true,
            "concurrencyLimit": 4294967295_u64,
            "weight": 100,
            "groupIds": []
        }))
        .expect("deserialize single update");
        request.validate().expect("validate single update");

        assert!(
            serde_json::from_value::<UpdateAccountRequest>(json!({
                "accountId": "acct_test",
                "enabled": true,
                "weight": 1,
                "groupIds": []
            }))
            .is_err()
        );
    }
}

mod response {
    use gateway_api::admin::accounts::AccountUsageView;
    use gateway_api::admin::presenter::format_decimal_currency;

    #[test]
    fn account_usage_view_should_keep_unobserved_numbers_null() {
        let view = AccountUsageView {
            request_count: None,
            request_count_display: "-".to_owned(),
            input_tokens: None,
            input_tokens_display: "-".to_owned(),
            output_tokens: None,
            output_tokens_display: "-".to_owned(),
            cached_tokens: None,
            cached_tokens_display: "-".to_owned(),
            image_input_tokens: None,
            image_input_tokens_display: "-".to_owned(),
            image_output_tokens: None,
            image_output_tokens_display: "-".to_owned(),
            image_request_count: None,
            image_request_count_display: "-".to_owned(),
            image_request_failed_count: None,
            image_request_failed_count_display: "-".to_owned(),
            total_tokens: None,
            total_tokens_display: "-".to_owned(),
            created_tokens: None,
            created_tokens_display: "-".to_owned(),
            read_tokens: None,
            read_tokens_display: "-".to_owned(),
            last_used_at: None,
            last_used_at_display: "-".to_owned(),
            cost_estimate_status: "unknown".to_owned(),
            known_cost_count: None,
            partial_cost_count: None,
            unknown_cost_count: None,
            costs: Vec::new(),
            models: Vec::new(),
        };
        let value = serde_json::to_value(view).expect("serialize account usage");
        assert!(value["inputTokens"].is_null());
        assert!(value["totalTokens"].is_null());
        assert_eq!(value["createdTokensDisplay"], "-");
    }

    #[test]
    fn account_usage_currency_should_limit_usd_to_four_fraction_digits() {
        assert_eq!(format_decimal_currency("0.1204956", "USD"), "$0.1205");
        assert_eq!(format_decimal_currency("0.99996", "USD"), "$1.00");
        assert_eq!(format_decimal_currency("0.0300", "USD"), "$0.03");
        assert_eq!(format_decimal_currency("12.3456", "USD"), "$12.35");
        assert_eq!(format_decimal_currency("0.1204956", "CNY"), "CNY 0.1204956");
    }
}

mod actions {
    use base64::Engine as _;
    use gateway_admin::model::{
        Revision,
        accounts::AccountConnectionTestEvent as DomainConnectionTestEvent,
        provider_credentials::{
            CredentialDeletionResult, CredentialImportResult, CredentialMutationResult,
        },
    };
    use gateway_api::admin::accounts::{
        AccountActionRequest, AccountConnectionTestEvent, AccountDeletionData,
        AccountDeletionRequest, AccountExportData, AccountExportQuery, AccountIdQuery,
        AccountImportData, AccountImportRequest, AccountMutationData, AccountRefreshRequest,
        AccountResetCreditConsumeRequest, AccountTestQuery, CompleteAccountAuthorizationRequest,
        RotateAccountRequest, StartAccountAuthorizationRequest,
    };
    use gateway_core::engine::credential::ProviderAccountId;
    use serde_json::json;

    #[test]
    fn export_should_require_explicit_unique_ids_and_confirmation() {
        let valid: AccountExportQuery = serde_json::from_value(json!({
            "accountIds": "acct_1,acct_2",
            "confirm": "export_sensitive_accounts"
        }))
        .expect("decode export query");
        assert_eq!(
            valid
                .into_ids()
                .expect("valid export")
                .into_iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>(),
            ["acct_1".to_owned(), "acct_2".to_owned()]
        );

        for query in [
            json!({ "accountIds": "", "confirm": "export_sensitive_accounts" }),
            json!({ "accountIds": "acct_1,acct_1", "confirm": "export_sensitive_accounts" }),
            json!({ "accountIds": "acct_1", "confirm": "yes" }),
        ] {
            assert!(
                serde_json::from_value::<AccountExportQuery>(query)
                    .expect("decode invalid export query")
                    .into_ids()
                    .is_err()
            );
        }
    }

    #[test]
    fn account_actions_should_require_frozen_account_ids_and_reject_unknown_revision() {
        let id: AccountIdQuery =
            serde_json::from_value(json!({ "accountId": "acct_1" })).expect("decode ID query");
        assert!(id.validate().is_ok());
        let action: AccountActionRequest =
            serde_json::from_value(json!({ "accountId": "legacy-id" })).expect("decode action");
        assert_eq!(action.validate().unwrap_err().field(), "accountId");
        let refresh: AccountRefreshRequest = serde_json::from_value(json!({
            "accountId": "acct_1"
        }))
        .expect("decode refresh");
        assert!(refresh.validate().is_ok());
        assert!(
            serde_json::from_value::<AccountRefreshRequest>(json!({
                "accountId": "acct_1",
                "expectedConfigRevision": 0
            }))
            .is_err()
        );
    }

    #[test]
    fn reset_credit_consume_should_require_canonical_v4_idempotency_key() {
        let valid: AccountResetCreditConsumeRequest = serde_json::from_value(json!({
            "accountId": "acct_1",
            "creditId": "credit_1",
            "redeemRequestId": "8fbf302d-11df-4bd5-82e4-08e4b3df7874"
        }))
        .expect("decode reset-credit consume");
        valid.validate().expect("validate reset-credit consume");

        for (redeem_request_id, field) in [
            ("019c0000-0000-7000-8000-000000000000", "redeemRequestId"),
            ("8FBF302D-11DF-4BD5-82E4-08E4B3DF7874", "redeemRequestId"),
            ("invalid", "redeemRequestId"),
        ] {
            let request: AccountResetCreditConsumeRequest = serde_json::from_value(json!({
                "accountId": "acct_1",
                "redeemRequestId": redeem_request_id
            }))
            .expect("decode invalid reset-credit consume");
            assert_eq!(request.validate().expect_err("reject UUID").field(), field);
        }

        let invalid_credit: AccountResetCreditConsumeRequest = serde_json::from_value(json!({
            "accountId": "acct_1",
            "creditId": " ",
            "redeemRequestId": "8fbf302d-11df-4bd5-82e4-08e4b3df7874"
        }))
        .expect("decode invalid credit");
        assert_eq!(
            invalid_credit
                .validate()
                .expect_err("reject credit")
                .field(),
            "creditId"
        );
    }

    #[test]
    fn credential_recovery_requests_should_not_accept_client_revision_fences() {
        let authorization: StartAccountAuthorizationRequest = serde_json::from_value(json!({
            "provider": "openai",
            "name": "reauthorize",
            "accountId": "acct_1"
        }))
        .expect("decode reauthorization");
        assert!(authorization.validate().is_ok());
        assert!(
            serde_json::from_value::<StartAccountAuthorizationRequest>(json!({
                "provider": "openai",
                "name": "reauthorize",
                "accountId": "acct_1",
                "expectedCredentialRevision": 1
            }))
            .is_err()
        );

        let rotation: RotateAccountRequest = serde_json::from_value(json!({
            "provider": "openai",
            "accountId": "acct_1",
            "accessToken": "header.payload.signature",
            "refreshToken": "refresh-token",
            "idToken": "id-header.id-payload.id-signature"
        }))
        .expect("decode rotation");
        assert!(rotation.validate().is_ok());
        assert!(
            serde_json::from_value::<RotateAccountRequest>(json!({
                "provider": "openai",
                "accountId": "acct_1",
                "expectedCredentialRevision": 1,
                "accessToken": "header.payload.signature"
            }))
            .is_err()
        );
    }

    #[test]
    fn credential_mutation_response_should_not_expose_internal_revision() {
        let response = AccountMutationData::from(CredentialMutationResult {
            config_revision: Revision::new(8).expect("config revision"),
            account_id: ProviderAccountId::new("acct_1").expect("account ID"),
            credential_revision: Some(Revision::new(9).expect("credential revision")),
        });

        assert_eq!(
            serde_json::to_value(response).expect("serialize credential mutation"),
            json!({ "accountId": "acct_1" })
        );
    }

    #[test]
    fn account_import_should_use_provider_and_opaque_data_fields() {
        let valid: AccountImportRequest = serde_json::from_value(json!({
            "provider": "openai",
            "data": {
                "providerOwnedUnknownField": {"nested": [1, 2, 3]},
                "accounts": [{"credentials": {"access_token": "provider-validates-this"}}]
            }
        }))
        .expect("decode account import");
        assert!(valid.validate().is_ok());

        let invalid: AccountImportRequest = serde_json::from_value(json!({
            "provider": "xai",
            "data": []
        }))
        .expect("decode invalid account import");
        assert_eq!(invalid.validate().unwrap_err().field(), "data");
        assert!(
            serde_json::from_value::<AccountImportRequest>(json!({
                "provider": "openai",
                "document": {}
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<AccountImportRequest>(json!({
                "provider": "openai",
                "expectedConfigRevision": 7,
                "data": {}
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<AccountImportRequest>(json!({
                "provider": "openai",
                "data": {},
                "groupIds": []
            }))
            .is_err()
        );
    }

    #[test]
    fn oauth_complete_should_not_accept_group_assignment() {
        let flow_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let request: CompleteAccountAuthorizationRequest = serde_json::from_value(json!({
            "provider": "openai",
            "flowId": flow_id,
            "callbackUrl": "http://localhost/callback?code=ok"
        }))
        .expect("decode OAuth completion");
        assert!(request.validate().is_ok());
        assert!(
            serde_json::from_value::<CompleteAccountAuthorizationRequest>(json!({
                "provider": "xai",
                "flowId": "flow_test",
                "callbackUrl": "http://localhost/callback?code=ok",
                "groupIds": []
            }))
            .is_err()
        );
    }

    #[test]
    fn account_deletion_should_validate_one_provider_batch_and_emit_account_ids() {
        let request: AccountDeletionRequest = serde_json::from_value(json!({
            "provider": "xai",
            "accountIds": ["acct_1", "acct_2"]
        }))
        .expect("decode account deletion");
        assert!(request.validate().is_ok());

        let duplicate: AccountDeletionRequest = serde_json::from_value(json!({
            "provider": "xai",
            "accountIds": ["acct_1", "acct_1"]
        }))
        .expect("decode duplicate account deletion");
        assert_eq!(duplicate.validate().unwrap_err().field(), "accountIds");

        let response = AccountDeletionData::from(CredentialDeletionResult {
            config_revision: Revision::new(9).expect("revision"),
            account_ids: vec![
                ProviderAccountId::new("acct_1").expect("account ID"),
                ProviderAccountId::new("acct_2").expect("account ID"),
            ],
        });
        assert_eq!(
            serde_json::to_value(response).expect("serialize account deletion"),
            json!({
                "deletedCount": 2,
                "accountIds": ["acct_1", "acct_2"]
            })
        );
    }

    #[test]
    fn account_import_response_should_emit_account_ids() {
        let response = AccountImportData::from_result(CredentialImportResult {
            config_revision: Revision::new(8).expect("revision"),
            credential_ids: vec![ProviderAccountId::new("acct_imported").expect("account ID")],
        });
        assert_eq!(
            serde_json::to_value(response).expect("serialize account import"),
            json!({
                "importedCount": 1,
                "accountIds": ["acct_imported"]
            })
        );
    }

    #[test]
    fn connection_test_should_require_model_in_query() {
        let query: AccountTestQuery = serde_json::from_value(json!({
            "accountId": "acct_1",
            "modelId": " "
        }))
        .expect("decode connection test query");
        assert_eq!(query.validate().unwrap_err().field(), "modelId");
    }

    #[test]
    fn connection_test_events_should_preserve_the_existing_frontend_contract() {
        let events = [
            DomainConnectionTestEvent::Started {
                model: "grok-4.5".to_owned(),
            },
            DomainConnectionTestEvent::Request {
                model: "grok-4.5".to_owned(),
                input_text: "Reply with exactly OK.".to_owned(),
                stream: true,
                store: false,
            },
            DomainConnectionTestEvent::Content {
                text: "OK".to_owned(),
            },
            DomainConnectionTestEvent::Completed {},
            DomainConnectionTestEvent::Failed {
                message: "upstream unavailable".to_owned(),
                provider_error_code: Some("usage_exhausted".to_owned()),
                provider_error_type: Some("invalid_request_error".to_owned()),
                upstream_status: Some(429),
                upstream_content_type: Some("application/json".to_owned()),
                upstream_body: Some(r#"{"error":{"type":"usage_limit_reached"}}"#.to_owned()),
            },
        ]
        .map(|event| AccountConnectionTestEvent::from(event).data);

        assert_eq!(
            events,
            [
                json!({ "type": "test_start", "model": "grok-4.5", "text": "正在连接上游 Responses" }),
                json!({
                    "type": "request",
                    "payload": {
                        "model": "grok-4.5",
                        "input": [{
                            "role": "user",
                            "content": [{
                                "type": "input_text",
                                "text": "Reply with exactly OK."
                            }]
                        }],
                        "stream": true,
                        "store": false
                    }
                }),
                json!({ "type": "content", "text": "OK" }),
                json!({ "type": "test_complete", "success": true }),
                json!({
                    "type": "error",
                    "error": "upstream unavailable",
                    "providerErrorCode": "usage_exhausted",
                    "providerErrorType": "invalid_request_error",
                    "upstreamStatus": 429,
                    "upstreamContentType": "application/json",
                    "upstreamBody": r#"{"error":{"type":"usage_limit_reached"}}"#
                }),
            ]
        );
    }

    #[test]
    fn provider_export_document_should_serialize_but_never_debug_secret() {
        let secret = "provider-refresh-token-must-not-enter-debug";
        let document = AccountExportData::new(json!({ "refresh_token": secret }));
        assert!(!format!("{document:?}").contains(secret));
        assert_eq!(
            serde_json::to_value(document).expect("serialize export"),
            json!({ "refresh_token": secret })
        );
    }
}
