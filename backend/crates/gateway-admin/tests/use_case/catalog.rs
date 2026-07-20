use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use gateway_core::routing::{ProviderInstanceId, ProviderKind};

use gateway_admin::{
    model::{
        MutationContext, PageSize, Revision,
        catalog::{
            CatalogListQuery, CreateProviderInstance, DeleteProviderInstance, ProviderInstance,
            ProviderInstanceCatalog, ProviderInstanceDetail, ProviderInstanceMutation,
            SetProviderInstanceEnabled, UpdateProviderInstance,
        },
    },
    ports::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult, CatalogStore},
};

struct CatalogFixtureStore {
    items: Vec<ProviderInstance>,
}

#[async_trait]
impl CatalogStore for CatalogFixtureStore {
    async fn list_provider_instances(&self, _: bool) -> AdminStoreResult<ProviderInstanceCatalog> {
        Ok(ProviderInstanceCatalog {
            config_revision: Revision::new(7).expect("revision"),
            items: self.items.clone(),
        })
    }

    async fn load_provider_instance(
        &self,
        id: &ProviderInstanceId,
    ) -> AdminStoreResult<Option<ProviderInstanceDetail>> {
        Ok(self
            .items
            .iter()
            .find(|item| &item.id == id)
            .cloned()
            .map(|item| ProviderInstanceDetail {
                config_revision: Revision::new(7).expect("revision"),
                item,
            }))
    }

    async fn create_provider_instance(
        &self,
        _: CreateProviderInstance,
        _: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        Err(unused())
    }

    async fn update_provider_instance(
        &self,
        _: UpdateProviderInstance,
        _: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        Err(unused())
    }

    async fn set_provider_instance_enabled(
        &self,
        _: SetProviderInstanceEnabled,
        _: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        Err(unused())
    }

    async fn delete_provider_instance(
        &self,
        _: DeleteProviderInstance,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unused())
    }
}

#[tokio::test]
async fn catalog_cursor_should_resume_after_exact_instance() {
    let items = ["inst_a", "inst_b", "inst_c"]
        .into_iter()
        .map(instance)
        .collect();
    let services = super::AdminHarness::new()
        .catalog(Arc::new(CatalogFixtureStore { items }))
        .build()
        .await;

    let page = services
        .catalog()
        .list(CatalogListQuery {
            cursor: Some(ProviderInstanceId::new("inst_a").expect("cursor")),
            page_size: PageSize::new(1).expect("page size"),
        })
        .await
        .expect("catalog page");

    assert_eq!(page.items[0].id.as_str(), "inst_b");
    assert_eq!(
        page.next_cursor.as_ref().map(ProviderInstanceId::as_str),
        Some("inst_b")
    );
}

#[tokio::test]
async fn catalog_detail_should_keep_its_control_plane_revision() {
    let services = super::AdminHarness::new()
        .catalog(Arc::new(CatalogFixtureStore {
            items: vec![instance("inst_detail")],
        }))
        .build()
        .await;

    let detail = services
        .catalog()
        .get(&ProviderInstanceId::new("inst_detail").expect("instance ID"))
        .await
        .expect("catalog detail");

    assert_eq!(detail.config_revision.get(), 7);
    assert_eq!(detail.item.id.as_str(), "inst_detail");
}

fn instance(id: &str) -> ProviderInstance {
    ProviderInstance {
        id: ProviderInstanceId::new(id).expect("instance ID"),
        provider_kind: ProviderKind::new("openai").expect("provider"),
        name: id.to_owned(),
        base_url: "https://example.com".to_owned(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn unused() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "catalog",
        "unused in this test",
    )
}
