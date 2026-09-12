use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use gateway_core::policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits};

use gateway_admin::{
    model::{
        AdminErrorKind, MutationActor, MutationContext, Revision,
        client_keys::{
            ClientKeyCursor, ClientKeyCursorValue, ClientKeyListQuery, ClientKeyPage,
            ClientKeyPageSize, ClientKeyRecord, ClientKeySecret, ClientKeySort, ClientKeySortField,
            CreateClientKey, DeleteClientKey, NewClientKey, SetClientKeyEnabled, SortDirection,
            UpdateClientKey,
        },
    },
    ports::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult, ClientKeyStore},
};

#[derive(Default)]
struct TestClientKeyStore {
    plaintexts: Mutex<Vec<String>>,
    create_error: Option<AdminStoreErrorKind>,
}

#[async_trait]
impl ClientKeyStore for TestClientKeyStore {
    async fn list_client_keys(&self, query: ClientKeyListQuery) -> AdminStoreResult<ClientKeyPage> {
        assert_eq!(query.page_size.get(), u16::MAX);
        Ok(ClientKeyPage {
            config_revision: Revision::new(1).expect("revision"),
            items: Vec::new(),
            total: 0,
            next_cursor: None,
        })
    }

    async fn reveal_client_key(
        &self,
        _: &ClientApiKeyId,
    ) -> AdminStoreResult<Option<ClientKeySecret>> {
        Err(unused())
    }

    async fn create_client_key(
        &self,
        key: NewClientKey,
        _: &MutationContext,
    ) -> AdminStoreResult<(Revision, ClientKeyRecord)> {
        if let Some(kind) = self.create_error {
            return Err(AdminStoreError::new(
                kind,
                "client key",
                "test create failure",
            ));
        }
        self.plaintexts.lock().unwrap().push(key.plaintext);
        let record = ClientKeyRecord {
            id: key.id,
            name: key.name,
            label: key.label,
            groups: Vec::new(),
            provider_kinds: Vec::new(),
            prefix: String::new(),
            enabled: true,
            limits: key.limits,
            budget: Default::default(),
            last_used_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        Ok((Revision::new(2).unwrap(), record))
    }

    async fn update_client_key(
        &self,
        _: UpdateClientKey,
        _: &MutationContext,
    ) -> AdminStoreResult<(Revision, ClientKeyRecord)> {
        Err(unused())
    }

    async fn set_client_key_enabled(
        &self,
        _: SetClientKeyEnabled,
        _: &MutationContext,
    ) -> AdminStoreResult<(Revision, ClientKeyRecord)> {
        Err(unused())
    }

    async fn delete_client_key(
        &self,
        _: DeleteClientKey,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unused())
    }
}

#[tokio::test]
async fn client_key_cursor_should_reject_value_that_does_not_match_sort() {
    let services = super::AdminHarness::new()
        .client_keys(Arc::new(TestClientKeyStore::default()))
        .build()
        .await;
    let sort = ClientKeySort {
        field: ClientKeySortField::Name,
        direction: SortDirection::Asc,
    };
    let error = services
        .client_keys()
        .list(ClientKeyListQuery {
            cursor: Some(ClientKeyCursor {
                sort,
                value: ClientKeyCursorValue::Enabled(true),
                id: ClientApiKeyId::new("key_cursor").expect("key ID"),
            }),
            page_size: ClientKeyPageSize::new(50).expect("page size"),
            search: None,
            sort,
        })
        .await
        .expect_err("mismatched cursor must fail");

    assert_eq!(error.kind(), AdminErrorKind::Invalid);
}

#[tokio::test]
async fn client_key_list_should_forward_the_full_nonzero_u16_page_size() {
    let services = super::AdminHarness::new()
        .client_keys(Arc::new(TestClientKeyStore::default()))
        .build()
        .await;
    let page = services
        .client_keys()
        .list(ClientKeyListQuery {
            cursor: None,
            page_size: ClientKeyPageSize::new(u16::MAX).expect("maximum page size"),
            search: None,
            sort: ClientKeySort {
                field: ClientKeySortField::CreatedAt,
                direction: SortDirection::Desc,
            },
        })
        .await
        .expect("maximum page size should reach store");

    assert_eq!(page.total, 0);
}

#[tokio::test]
async fn create_preserves_migrated_keys_and_keeps_default_generation() {
    let store = Arc::new(TestClientKeyStore::default());
    let services = super::AdminHarness::new()
        .client_keys(store.clone())
        .build()
        .await;
    for value in [Some("q".to_owned()), Some("legacy+/=:!".repeat(1024)), None] {
        let created = services
            .client_keys()
            .create(&mutation_context(), create_command(value.as_deref()))
            .await
            .unwrap();
        let plaintext = created.secret.expose_for_response();
        assert_eq!(store.plaintexts.lock().unwrap().last().unwrap(), plaintext);
        if let Some(value) = value {
            assert_eq!(plaintext, value);
        } else {
            assert!(plaintext.starts_with("sk_"));
            assert_eq!(plaintext.len(), 46);
        }
    }
}

#[tokio::test]
async fn duplicate_keys_return_actionable_conflicts_without_disclosing_the_key() {
    let services = super::AdminHarness::new()
        .client_keys(Arc::new(TestClientKeyStore {
            create_error: Some(AdminStoreErrorKind::Conflict),
            ..Default::default()
        }))
        .build()
        .await;
    let key = "legacy-duplicate-must-stay-private";
    let error = services
        .client_keys()
        .create(&mutation_context(), create_command(Some(key)))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), AdminErrorKind::Conflict);
    assert_eq!(error.message(), "API Key 已存在，请使用其他密钥");
    assert!(!format!("{error:?}").contains(key));
}

fn create_command(key: Option<&str>) -> CreateClientKey {
    CreateClientKey {
        custom_key: key.map(|value| PlaintextClientApiKey::new(value).unwrap()),
        name: "Migration".to_owned(),
        label: None,
        group_ids: Vec::new(),
        limits: RateLimits::unlimited(),
        budget: Default::default(),
    }
}

fn mutation_context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "custom-key-test".to_owned(),
    }
}

fn unused() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "client key",
        "unused in this test",
    )
}
