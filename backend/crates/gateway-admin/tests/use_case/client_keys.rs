//! Client Key 管理、预算变更与存储调用的用例测试

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use gateway_core::policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits};

use gateway_admin::{
    model::{
        AdminErrorKind, MutationActor, MutationContext, PageSize, Revision,
        client_keys::{
            ClientKeyCursor, ClientKeyCursorValue, ClientKeyListQuery, ClientKeyPage,
            ClientKeyPageSize, ClientKeyRecord, ClientKeySecret, ClientKeySort, ClientKeySortField,
            CreateClientKey, DeleteClientKey, NewClientKey, SetClientKeyEnabled, SortDirection,
            UpdateClientKey,
        },
        plugin_client_keys::{
            PluginClientKey, PluginClientKeyCursor, PluginClientKeyListQuery, PluginClientKeyPage,
        },
    },
    ports::provider::{ProviderAdmin, ProviderAdminRegistry},
    ports::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult, ClientKeyStore},
};

#[derive(Default)]
struct TestClientKeyStore {
    plaintexts: Mutex<Vec<String>>,
    list_queries: Mutex<Vec<ClientKeyListQuery>>,
    list_response: Mutex<Option<ClientKeyPage>>,
    create_error: Option<AdminStoreErrorKind>,
    update_error: Option<AdminStoreErrorKind>,
    resets: Mutex<Vec<gateway_admin::model::client_keys::ResetClientKeyBudget>>,
}

#[async_trait]
impl ClientKeyStore for TestClientKeyStore {
    async fn update_client_key_budget_limits(
        &self,
        _: gateway_admin::model::client_keys::UpdateClientKeyBudgetLimits,
        _: gateway_admin::model::client_keys::ClientKeyBudgetMutationOrigin,
        _: &MutationContext,
    ) -> AdminStoreResult<Option<Revision>> {
        Err(unused())
    }

    async fn reset_client_key_budget(
        &self,
        command: gateway_admin::model::client_keys::ResetClientKeyBudget,
        _: gateway_admin::model::client_keys::ClientKeyBudgetMutationOrigin,
        _: &MutationContext,
    ) -> AdminStoreResult<()> {
        self.resets.lock().unwrap().push(command);
        Ok(())
    }

    async fn get_client_key(
        &self,
        _: &ClientApiKeyId,
    ) -> AdminStoreResult<Option<ClientKeyRecord>> {
        Err(unused())
    }

    async fn list_client_keys(&self, query: ClientKeyListQuery) -> AdminStoreResult<ClientKeyPage> {
        self.list_queries.lock().unwrap().push(query.clone());
        if let Some(page) = self.list_response.lock().unwrap().clone() {
            return Ok(page);
        }
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
            request_profile_overrides: Default::default(),
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
        if let Some(kind) = self.update_error {
            return Err(AdminStoreError::new(
                kind,
                "client key",
                "test update failure",
            ));
        }
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
async fn reset_budget_forwards_scope_and_returns_only_key_identity() {
    use gateway_admin::model::client_keys::{ClientKeyBudgetPeriod, ResetClientKeyBudget};
    let store = Arc::new(TestClientKeyStore::default());
    let services = super::AdminHarness::new()
        .client_keys(store.clone())
        .build()
        .await;
    let command = ResetClientKeyBudget {
        id: ClientApiKeyId::new("key_reset").unwrap(),
        period: ClientKeyBudgetPeriod::Weekly,
    };
    let result = services
        .client_keys()
        .reset_budget(
            &mutation_context(),
            command.clone(),
            gateway_admin::model::client_keys::ClientKeyBudgetMutationOrigin::Admin,
        )
        .await
        .unwrap();
    assert_eq!(result, command.id);
    assert_eq!(*store.resets.lock().unwrap(), vec![command]);
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
async fn plugin_client_key_list_projects_only_public_identity_and_uses_stable_name_cursor() {
    let now = Utc::now();
    let key_id = ClientApiKeyId::new("key_public").expect("key ID");
    let next_id = ClientApiKeyId::new("key_next").expect("next key ID");
    let sort = ClientKeySort {
        field: ClientKeySortField::Name,
        direction: SortDirection::Asc,
    };
    let store = Arc::new(TestClientKeyStore {
        list_response: Mutex::new(Some(ClientKeyPage {
            config_revision: Revision::new(7).expect("revision"),
            items: vec![ClientKeyRecord {
                request_profile_overrides: Default::default(),
                id: key_id.clone(),
                name: "Public identity".to_owned(),
                label: Some("private management label".to_owned()),
                groups: Vec::new(),
                provider_kinds: Vec::new(),
                prefix: "sk_must_not_escape".to_owned(),
                enabled: true,
                limits: RateLimits::unlimited(),
                budget: Default::default(),
                last_used_at: Some(now),
                created_at: now,
                updated_at: now,
            }],
            total: 1,
            next_cursor: Some(ClientKeyCursor {
                sort,
                value: ClientKeyCursorValue::Name("Public identity".to_owned()),
                id: next_id.clone(),
            }),
        })),
        ..Default::default()
    });
    let providers = ProviderAdminRegistry::new(Vec::<Arc<dyn ProviderAdmin>>::new())
        .expect("empty provider registry");
    let access = gateway_admin::initialize_plugin_client_keys(
        providers,
        store.clone(),
        Arc::new(super::NoopSnapshot),
    );
    let input_cursor = PluginClientKeyCursor {
        name: "Before".to_owned(),
        id: ClientApiKeyId::new("key_before").expect("cursor key ID"),
    };

    let page = access
        .list(PluginClientKeyListQuery {
            cursor: Some(input_cursor.clone()),
            limit: PageSize::new(25).expect("page size"),
        })
        .await
        .expect("list plugin client keys");

    assert_eq!(
        page,
        PluginClientKeyPage {
            items: vec![PluginClientKey {
                id: key_id,
                name: "Public identity".to_owned(),
                enabled: true,
            }],
            next_cursor: Some(PluginClientKeyCursor {
                name: "Public identity".to_owned(),
                id: next_id,
            }),
        }
    );
    assert_eq!(
        *store.list_queries.lock().unwrap(),
        vec![ClientKeyListQuery {
            cursor: Some(ClientKeyCursor {
                sort,
                value: ClientKeyCursorValue::Name(input_cursor.name),
                id: input_cursor.id,
            }),
            page_size: ClientKeyPageSize::new(25).expect("page size"),
            search: None,
            sort,
        }]
    );
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

#[tokio::test]
async fn duplicate_names_report_the_same_actionable_conflict_on_create_and_update() {
    let services = super::AdminHarness::new()
        .client_keys(Arc::new(TestClientKeyStore {
            create_error: Some(AdminStoreErrorKind::DuplicateName),
            update_error: Some(AdminStoreErrorKind::DuplicateName),
            ..Default::default()
        }))
        .build()
        .await;
    let created = services
        .client_keys()
        .create(&mutation_context(), create_command(None))
        .await
        .unwrap_err();
    let updated = services
        .client_keys()
        .update(
            &mutation_context(),
            UpdateClientKey {
                request_profile_override_updates: Default::default(),
                id: ClientApiKeyId::new("key_existing").unwrap(),
                name: "Migration".to_owned(),
                label: None,
                group_ids: Vec::new(),
                limits: RateLimits::unlimited(),
                daily_limit_usd: None,
                weekly_limit_usd: None,
            },
        )
        .await
        .unwrap_err();
    for error in [created, updated] {
        assert_eq!(error.kind(), AdminErrorKind::Conflict);
        assert_eq!(error.message(), "名称已存在");
    }
}

fn create_command(key: Option<&str>) -> CreateClientKey {
    CreateClientKey {
        request_profile_overrides: Default::default(),
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
