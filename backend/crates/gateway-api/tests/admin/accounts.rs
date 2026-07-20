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
            "status": "active",
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
        assert_eq!(query.status, Some(AccountStatus::Active));
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

mod response {
    use gateway_api::admin::accounts::AccountUsageView;

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
}

mod actions {
    use gateway_admin::model::accounts::{
        AccountConnectionTestEvent as DomainConnectionTestEvent, AccountStatus,
    };
    use gateway_api::admin::accounts::{
        AccountActionRequest, AccountConnectionTestEvent, AccountExportData, AccountExportQuery,
        AccountIdQuery, AccountRefreshRequest, AccountTestQuery,
    };
    use serde_json::json;

    #[test]
    fn export_should_require_explicit_unique_ids_and_confirmation() {
        let valid: AccountExportQuery = serde_json::from_value(json!({
            "ids": "acct_1,acct_2",
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
            json!({ "ids": "", "confirm": "export_sensitive_accounts" }),
            json!({ "ids": "acct_1,acct_1", "confirm": "export_sensitive_accounts" }),
            json!({ "ids": "acct_1", "confirm": "yes" }),
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
    fn account_actions_should_require_frozen_account_ids_and_revision() {
        let id: AccountIdQuery =
            serde_json::from_value(json!({ "id": "acct_1" })).expect("decode ID query");
        assert!(id.validate().is_ok());
        let action: AccountActionRequest =
            serde_json::from_value(json!({ "id": "legacy-id" })).expect("decode action");
        assert_eq!(action.validate().unwrap_err().field(), "id");
        let refresh: AccountRefreshRequest = serde_json::from_value(json!({
            "id": "acct_1",
            "expectedConfigRevision": 0
        }))
        .expect("decode refresh");
        assert_eq!(
            refresh.validate().unwrap_err().field(),
            "expectedConfigRevision"
        );
    }

    #[test]
    fn connection_test_should_require_model_in_query() {
        let query: AccountTestQuery = serde_json::from_value(json!({
            "id": "acct_1",
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
            DomainConnectionTestEvent::Completed {
                account_status: AccountStatus::Active,
            },
            DomainConnectionTestEvent::Failed {
                message: "upstream unavailable".to_owned(),
                account_status: AccountStatus::Active,
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
                json!({ "type": "test_complete", "success": true, "accountStatus": "active" }),
                json!({ "type": "error", "error": "upstream unavailable", "accountStatus": "active" }),
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
