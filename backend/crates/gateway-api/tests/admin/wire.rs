mod common {
    use gateway_api::admin::PageMeta;
    use serde_json::json;

    #[test]
    fn page_meta_should_keep_the_existing_camel_case_wire_shape() {
        let value = serde_json::to_value(PageMeta::new(2, 10, 21, 3))
            .expect("page metadata should serialize");

        assert_eq!(
            value,
            json!({
                "page": 2,
                "pageSize": 10,
                "total": 21,
                "totalPages": 3
            })
        );
    }
}

mod error {
    use gateway_api::admin::{AdminErrorBody, AdminErrorCode};
    use serde_json::json;

    #[test]
    fn error_codes_should_keep_all_published_numeric_values() {
        let actual = [
            AdminErrorCode::MALFORMED_JSON,
            AdminErrorCode::BAD_REQUEST,
            AdminErrorCode::INVALID_TIME_RANGE,
            AdminErrorCode::INVALID_MODEL_SOURCE,
            AdminErrorCode::SESSION_REQUIRED,
            AdminErrorCode::INVALID_CREDENTIALS,
            AdminErrorCode::INVALID_API_KEY,
            AdminErrorCode::NOT_FOUND,
            AdminErrorCode::CONFLICT,
            AdminErrorCode::TOO_MANY_LOGIN_ATTEMPTS,
            AdminErrorCode::SETTINGS_PERSIST,
            AdminErrorCode::INTERNAL,
            AdminErrorCode::USAGE_RECORD_ACCOUNTS,
            AdminErrorCode::BAD_GATEWAY,
            AdminErrorCode::SERVICE_UNAVAILABLE,
        ]
        .map(AdminErrorCode::value);

        assert_eq!(
            actual,
            [
                40000, 40001, 40002, 40003, 40101, 40102, 40103, 40401, 40901, 42901, 50000, 50001,
                50002, 50201, 50301,
            ]
        );
    }

    #[test]
    fn invalid_admin_api_key_should_keep_code_and_null_data() {
        let value = serde_json::to_value(AdminErrorBody::new(
            AdminErrorCode::INVALID_API_KEY,
            "Invalid admin API key",
        ))
        .expect("admin error body should serialize");

        assert_eq!(
            value,
            json!({
                "code": 40103,
                "message": "Invalid admin API key",
                "data": null
            })
        );
    }
}

mod response {
    use axum::{
        body::to_bytes,
        http::StatusCode,
        response::{IntoResponse, Response},
    };
    use gateway_api::admin::{AdminEnvelope, AdminError, AdminPageData, AdminResponse, PageMeta};
    use serde_json::json;

    #[test]
    fn success_envelope_should_keep_the_existing_wire_shape() {
        let value = serde_json::to_value(AdminEnvelope::ok(json!({ "id": "resource_1" })))
            .expect("admin success envelope should serialize");

        assert_eq!(
            value,
            json!({
                "code": 200,
                "message": "OK",
                "data": { "id": "resource_1" }
            })
        );
    }

    #[test]
    fn page_envelope_should_keep_items_and_page_nested_under_data() {
        let data = AdminPageData::new(
            vec![json!({ "id": "resource_1" })],
            PageMeta::new(2, 10, 21, 3),
        );
        let value = serde_json::to_value(AdminEnvelope::ok(data))
            .expect("admin page envelope should serialize");

        assert_eq!(
            value,
            json!({
                "code": 200,
                "message": "OK",
                "data": {
                    "items": [{ "id": "resource_1" }],
                    "page": {
                        "page": 2,
                        "pageSize": 10,
                        "total": 21,
                        "totalPages": 3
                    }
                }
            })
        );
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("read admin response body");
        serde_json::from_slice(&body).expect("parse admin response body")
    }

    #[tokio::test]
    async fn admin_response_should_keep_http_status_independent_from_envelope_code() {
        let response: Response = AdminResponse::new(
            StatusCode::CREATED,
            AdminEnvelope::ok(json!({ "id": "resource_created" })),
        )
        .into_response();
        let status = response.status();
        let body = response_json(response).await;

        assert_eq!(
            (status, body["code"].as_u64()),
            (StatusCode::CREATED, Some(200))
        );
    }

    #[tokio::test]
    async fn invalid_admin_api_key_should_keep_its_published_wire_contract() {
        let response = AdminError::invalid_admin_api_key().into_response();
        let status = response.status();
        let body = response_json(response).await;

        assert_eq!(
            (status, body),
            (
                StatusCode::UNAUTHORIZED,
                json!({
                    "code": 40103,
                    "message": "Invalid admin API key",
                    "data": null
                })
            )
        );
    }

    #[tokio::test]
    async fn admin_error_variants_should_use_stable_http_and_null_data_contract() {
        let cases = [
            (
                AdminError::bad_request("bad input"),
                StatusCode::BAD_REQUEST,
                40001,
            ),
            (
                AdminError::admin_session_required(),
                StatusCode::UNAUTHORIZED,
                40101,
            ),
            (
                AdminError::invalid_admin_credentials(),
                StatusCode::UNAUTHORIZED,
                40102,
            ),
            (
                AdminError::too_many_login_attempts(),
                StatusCode::TOO_MANY_REQUESTS,
                42901,
            ),
            (
                AdminError::conflict("conflict"),
                StatusCode::CONFLICT,
                40901,
            ),
            (
                AdminError::not_found("missing"),
                StatusCode::NOT_FOUND,
                40401,
            ),
            (
                AdminError::internal("internal"),
                StatusCode::INTERNAL_SERVER_ERROR,
                50001,
            ),
            (
                AdminError::bad_gateway("upstream"),
                StatusCode::BAD_GATEWAY,
                50201,
            ),
            (
                AdminError::service_unavailable("storage"),
                StatusCode::SERVICE_UNAVAILABLE,
                50301,
            ),
        ];

        for (error, expected_status, expected_code) in cases {
            let response = error.into_response();
            let status = response.status();
            let body = response_json(response).await;

            assert_eq!(status, expected_status);
            assert_eq!(body["code"], expected_code);
            assert!(body["message"].is_string());
            assert!(body["data"].is_null());
        }
    }
}
