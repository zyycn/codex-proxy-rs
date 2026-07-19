use chrono::{DateTime, TimeZone as _, Utc};
use gateway_api::admin::client_keys::{
    self, ClientKeyAdminService, ClientKeyAdminState, ClientKeyCursorData, ClientKeyCursorValue,
    ClientKeyListData, ClientKeyRevisionRequest, ClientKeySort, ClientKeySortDirection,
    ClientKeySortField, ClientKeyView, ClientKeyViewFields, CreateClientKeyFields,
    CreateClientKeyRequest, CreatedClientKeyData, ListClientKeysFields, ListClientKeysQuery,
    MutatedClientKeyData, RevealedClientKeyData, UpdateClientKeyFields, UpdateClientKeyRequest,
    decode_client_key_cursor, encode_client_key_cursor,
};
use gateway_api::admin::{
    AdminRequestContext, AdminServiceError, AdminSessionResolver, AdminSessionState,
};
use serde_json::json;

#[test]
fn client_key_queries_should_reject_unknown_zero_and_oversized_fields() {
    let unknown = serde_json::from_value::<ListClientKeysQuery>(json!({ "other": true }));
    let zero = serde_json::from_value::<ListClientKeysQuery>(json!({ "limit": 0 }))
        .expect("deserialize zero limit");
    let oversized = serde_json::from_value::<ListClientKeysQuery>(json!({
        "cursor": "a".repeat(513)
    }))
    .expect("deserialize oversized cursor");
    let oversized_search = serde_json::from_value::<ListClientKeysQuery>(json!({
        "search": "a".repeat(257)
    }))
    .expect("deserialize oversized search");
    let secret_search = serde_json::from_value::<ListClientKeysQuery>(json!({
        "search": format!("sk_{}", "a".repeat(43))
    }))
    .expect("deserialize secret search");
    let invalid_sort = serde_json::from_value::<ListClientKeysQuery>(json!({
        "sortBy": "plaintextKey",
        "sortDirection": "asc"
    }))
    .expect("deserialize invalid sort");

    assert!(unknown.is_err());
    assert_eq!(
        zero.into_parts().expect_err("reject zero limit").field(),
        "limit"
    );
    assert_eq!(
        oversized
            .into_parts()
            .expect_err("reject oversized cursor")
            .field(),
        "cursor"
    );
    assert_eq!(
        oversized_search
            .into_parts()
            .expect_err("reject oversized search")
            .field(),
        "search"
    );
    assert_eq!(
        secret_search
            .into_parts()
            .expect_err("reject full key in search")
            .field(),
        "search"
    );
    assert_eq!(
        invalid_sort
            .into_parts()
            .expect_err("reject invalid sort")
            .field(),
        "sortBy"
    );
    let sorted = serde_json::from_value::<ListClientKeysQuery>(json!({
        "sortBy": "lastUsedAt",
        "sortDirection": "asc"
    }))
    .expect("deserialize valid sort")
    .into_parts()
    .expect("validate sort");
    assert_eq!(
        sorted.sort,
        ClientKeySort {
            field: ClientKeySortField::LastUsedAt,
            direction: ClientKeySortDirection::Asc,
        }
    );
    assert_eq!(
        serde_json::from_value::<ListClientKeysQuery>(json!({ "search": "  " }))
            .expect("deserialize blank search")
            .into_parts()
            .expect("normalize blank search")
            .search,
        None
    );
}

#[test]
fn client_key_mutations_should_validate_revision_text_limits_and_unknown_fields() {
    let valid = serde_json::from_value::<CreateClientKeyRequest>(json!({
        "expectedConfigRevision": 7,
        "name": "terminal key",
        "label": "production",
        "providerKind": "openai",
        "maxConcurrency": 2,
        "requestsPerMinute": 60,
        "tokensPerMinute": 100000
    }))
    .expect("deserialize create")
    .into_fields()
    .expect("validate create");
    assert_eq!(valid.expected_config_revision, 7);

    for (payload, field) in [
        (
            json!({
                "expectedConfigRevision": 0,
                "name": "key",
                "providerKind": "openai",
                "maxConcurrency": 0,
                "requestsPerMinute": 0,
                "tokensPerMinute": 0
            }),
            "expectedConfigRevision",
        ),
        (
            json!({
                "expectedConfigRevision": 1,
                "name": " ",
                "providerKind": "openai",
                "maxConcurrency": 0,
                "requestsPerMinute": 0,
                "tokensPerMinute": 0
            }),
            "name",
        ),
        (
            json!({
                "expectedConfigRevision": 1,
                "name": "key",
                "providerKind": "openai",
                "maxConcurrency": u64::MAX,
                "requestsPerMinute": 0,
                "tokensPerMinute": 0
            }),
            "maxConcurrency",
        ),
    ] {
        let request = serde_json::from_value::<CreateClientKeyRequest>(payload)
            .expect("deserialize invalid create shape");
        assert_eq!(
            request
                .into_fields()
                .expect_err("reject invalid create")
                .field(),
            field
        );
    }

    assert!(
        serde_json::from_value::<UpdateClientKeyRequest>(json!({
            "id": "key_1",
            "expectedConfigRevision": 1,
            "name": "key",
            "providerKind": "openai",
            "maxConcurrency": 0,
            "requestsPerMinute": 0,
            "tokensPerMinute": 0,
            "plaintextKey": "must-not-be-accepted"
        }))
        .is_err()
    );
    let revision = serde_json::from_value::<ClientKeyRevisionRequest>(json!({
        "id": "key_1",
        "expectedConfigRevision": 3
    }))
    .expect("deserialize revision mutation")
    .into_parts()
    .expect("validate revision mutation");
    assert_eq!(revision, ("key_1".to_owned(), 3));
}

#[test]
fn client_key_cursor_should_round_trip_and_reject_noncanonical_input() {
    let created_at = Utc
        .with_ymd_and_hms(2026, 7, 18, 8, 0, 0)
        .single()
        .expect("valid time");
    let cursor = ClientKeyCursorData {
        sort: ClientKeySort {
            field: ClientKeySortField::CreatedAt,
            direction: ClientKeySortDirection::Desc,
        },
        value: ClientKeyCursorValue::CreatedAt(created_at),
        id: "key_cursor".to_owned(),
    };
    let encoded = encode_client_key_cursor(&cursor).expect("encode cursor");
    let decoded = decode_client_key_cursor(&encoded).expect("decode cursor");

    assert_eq!(decoded, cursor);
    assert!(!encoded.contains("key_cursor"));
    for invalid in ["", "not-base64!", "e30"] {
        assert!(decode_client_key_cursor(invalid).is_err());
    }
}

#[test]
fn client_key_responses_should_keep_shape_and_redact_creation_debug() {
    let created_at = Utc
        .with_ymd_and_hms(2026, 7, 18, 8, 0, 0)
        .single()
        .expect("valid time");
    let view = ClientKeyView::new(ClientKeyViewFields {
        id: "key_visible".to_owned(),
        name: "visible".to_owned(),
        label: None,
        provider_kind: "openai".to_owned(),
        prefix: "sk_visible12".to_owned(),
        enabled: true,
        max_concurrency: 2,
        requests_per_minute: 60,
        tokens_per_minute: 100_000,
        created_at,
        updated_at: created_at,
        last_used_at: Some(created_at),
    });
    let list = serde_json::to_value(ClientKeyListData::new(9, vec![view], None, 1))
        .expect("serialize list");
    assert_eq!(list["configRevision"], 9);
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], "key_visible");
    assert_eq!(list["items"][0]["providerKind"], "openai");
    assert_eq!(list["items"][0]["maxConcurrency"], 2);
    assert_eq!(list["items"][0]["requestsPerMinute"], 60);
    assert_eq!(list["items"][0]["tokensPerMinute"], 100_000);
    assert!(list["items"][0].get("policyJson").is_none());
    assert_eq!(
        DateTime::parse_from_rfc3339(
            list["items"][0]["lastUsedAt"]
                .as_str()
                .expect("serialized last-used timestamp")
        )
        .expect("parse serialized last-used timestamp"),
        created_at.fixed_offset()
    );
    assert!(list["items"][0].get("plaintextKey").is_none());

    let plaintext = format!("sk_{}", "a".repeat(43));
    let created = CreatedClientKeyData::new(
        10,
        "key_created".to_owned(),
        "sk_aaaaaaaaa".to_owned(),
        plaintext.clone(),
    );
    assert!(!format!("{created:?}").contains(&plaintext));
    let value = serde_json::to_value(created).expect("serialize created key");
    assert_eq!(value["plaintextKey"], plaintext);
    let revealed = RevealedClientKeyData::new("key_created".to_owned(), plaintext.clone());
    assert!(!format!("{revealed:?}").contains(&plaintext));
    assert_eq!(
        serde_json::to_value(revealed).expect("serialize revealed key"),
        json!({ "id": "key_created", "plaintextKey": plaintext })
    );
    assert_eq!(
        serde_json::to_value(MutatedClientKeyData::new(11, "key_created".to_owned()))
            .expect("serialize mutation"),
        json!({ "configRevision": 11, "id": "key_created" })
    );
}

struct RevealService;

#[async_trait::async_trait]
impl AdminSessionResolver for RevealService {
    async fn resolve_admin_user_id(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<String>, AdminServiceError> {
        Ok((session_id == Some("valid-session")).then(|| "admin_1".to_owned()))
    }
}

#[async_trait::async_trait]
impl ClientKeyAdminService for RevealService {
    async fn list(
        &self,
        _query: ListClientKeysFields,
    ) -> Result<ClientKeyListData, AdminServiceError> {
        Err(AdminServiceError::internal("unused test operation"))
    }

    async fn create(
        &self,
        _context: &AdminRequestContext,
        _fields: CreateClientKeyFields,
    ) -> Result<CreatedClientKeyData, AdminServiceError> {
        Err(AdminServiceError::internal("unused test operation"))
    }

    async fn reveal(&self, id: String) -> Result<RevealedClientKeyData, AdminServiceError> {
        Ok(RevealedClientKeyData::new(
            id,
            format!("sk_{}", "a".repeat(43)),
        ))
    }

    async fn update(
        &self,
        _context: &AdminRequestContext,
        _fields: UpdateClientKeyFields,
    ) -> Result<MutatedClientKeyData, AdminServiceError> {
        Err(AdminServiceError::internal("unused test operation"))
    }

    async fn disable(
        &self,
        _context: &AdminRequestContext,
        _id: String,
        _expected_config_revision: u64,
    ) -> Result<MutatedClientKeyData, AdminServiceError> {
        Err(AdminServiceError::internal("unused test operation"))
    }

    async fn enable(
        &self,
        _context: &AdminRequestContext,
        _id: String,
        _expected_config_revision: u64,
    ) -> Result<MutatedClientKeyData, AdminServiceError> {
        Err(AdminServiceError::internal("unused test operation"))
    }

    async fn delete(
        &self,
        _context: &AdminRequestContext,
        _id: String,
        _expected_config_revision: u64,
    ) -> Result<MutatedClientKeyData, AdminServiceError> {
        Err(AdminServiceError::internal("unused test operation"))
    }
}

#[derive(Clone)]
struct RevealState(std::sync::Arc<RevealService>);

impl AdminSessionState for RevealState {
    fn admin_session_resolver(&self) -> &dyn AdminSessionResolver {
        self.0.as_ref()
    }
}

impl ClientKeyAdminState for RevealState {
    fn client_key_admin_service(&self) -> &dyn ClientKeyAdminService {
        self.0.as_ref()
    }
}

#[tokio::test]
async fn reveal_route_should_use_query_id_and_no_store_response() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt as _;

    let state = RevealState(std::sync::Arc::new(RevealService));
    let response = client_keys::router::<RevealState>()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/api/admin/client-keys/reveal?id=key_1")
                .header(header::COOKIE, "cpr_admin_session=valid-session")
                .header("x-request-id", "req_reveal")
                .body(Body::empty())
                .expect("reveal request"),
        )
        .await
        .expect("reveal response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let body = to_bytes(response.into_body(), 4096)
        .await
        .expect("reveal body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("reveal JSON");
    assert_eq!(value["data"]["id"], "key_1");
    assert_eq!(
        value["data"]["plaintextKey"],
        format!("sk_{}", "a".repeat(43))
    );
}
