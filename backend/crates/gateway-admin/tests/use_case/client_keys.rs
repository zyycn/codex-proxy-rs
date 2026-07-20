use async_trait::async_trait;
use gateway_core::policy::ClientApiKeyId;

use gateway_admin::{
    model::{
        AdminErrorKind, MutationContext, Revision,
        client_keys::{
            ClientKeyCursor, ClientKeyCursorValue, ClientKeyListQuery, ClientKeyPage,
            ClientKeyPageSize, ClientKeyRecord, ClientKeySecret, ClientKeySort, ClientKeySortField,
            DeleteClientKey, NewClientKey, SetClientKeyEnabled, SortDirection, UpdateClientKey,
        },
    },
    ports::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult, ClientKeyStore},
};

struct UnusedClientKeyStore;

#[async_trait]
impl ClientKeyStore for UnusedClientKeyStore {
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
        _: NewClientKey,
        _: &MutationContext,
    ) -> AdminStoreResult<(Revision, ClientKeyRecord)> {
        Err(unused())
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
        .client_keys(std::sync::Arc::new(UnusedClientKeyStore))
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
        .client_keys(std::sync::Arc::new(UnusedClientKeyStore))
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

fn unused() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "client key",
        "unused in this test",
    )
}
