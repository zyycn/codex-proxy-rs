//! 客户端 API Key 管理服务。

use crate::infra::json::NumberedPage;

use super::{
    store::PgClientKeyStore,
    types::{
        BatchDeleteClientApiKeys, KeyManageError, ManagedClientApiKey, parse_client_key_status,
    },
};

#[derive(Clone)]
pub struct KeyManageService {
    store: PgClientKeyStore,
}

impl KeyManageService {
    pub fn new(store: PgClientKeyStore) -> Self {
        Self { store }
    }

    pub async fn create(&self, name: &str) -> Result<ManagedClientApiKey, KeyManageError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(KeyManageError::EmptyName);
        }
        self.store
            .create(name)
            .await
            .map(Into::into)
            .map_err(|_| KeyManageError::Create)
    }

    pub async fn list_page(
        &self,
        page: u32,
        page_size: u32,
        search: Option<String>,
    ) -> Result<NumberedPage<ManagedClientApiKey>, KeyManageError> {
        let page = self
            .store
            .list_page(page, page_size, search.as_deref())
            .await
            .map_err(|_| KeyManageError::List)?;
        Ok(NumberedPage {
            items: page.items.into_iter().map(Into::into).collect(),
            total: page.total,
            page: page.page,
            page_size: page.page_size,
        })
    }

    pub async fn get(&self, key_id: &str) -> Result<Option<ManagedClientApiKey>, KeyManageError> {
        self.store
            .get(key_id)
            .await
            .map(|key| key.map(Into::into))
            .map_err(|_| KeyManageError::List)
    }

    pub async fn update_label(
        &self,
        key_id: &str,
        label: Option<String>,
    ) -> Result<Option<ManagedClientApiKey>, KeyManageError> {
        if label
            .as_ref()
            .is_some_and(|value| value.chars().count() > 64)
        {
            return Err(KeyManageError::LabelTooLong);
        }
        self.store
            .set_label(key_id, label)
            .await
            .map(|key| key.map(Into::into))
            .map_err(|_| KeyManageError::UpdateLabel)
    }

    pub async fn update_status(&self, key_id: &str, status: &str) -> Result<bool, KeyManageError> {
        self.store
            .set_enabled(key_id, parse_client_key_status(status)?)
            .await
            .map_err(|_| KeyManageError::UpdateStatus)
    }

    pub async fn batch_delete(
        &self,
        ids: Vec<String>,
    ) -> Result<BatchDeleteClientApiKeys, KeyManageError> {
        if ids.is_empty() {
            return Err(KeyManageError::EmptyIds);
        }
        let mut deleted = 0u32;
        let mut not_found = Vec::new();
        for id in ids {
            match self.store.delete(&id).await {
                Ok(true) => deleted += 1,
                Ok(false) => not_found.push(id),
                Err(_) => return Err(KeyManageError::Delete),
            }
        }
        Ok(BatchDeleteClientApiKeys { deleted, not_found })
    }
}
