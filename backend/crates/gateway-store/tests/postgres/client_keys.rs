use gateway_admin::{
    model::client_keys::{
        ClientKeyListQuery as AdminClientKeyListQuery, ClientKeyPageSize,
        ClientKeySort as AdminClientKeySort, ClientKeySortField as AdminClientKeySortField,
        SortDirection as AdminSortDirection,
    },
    ports::store::ClientKeyStore as _,
};
use gateway_store::postgres::{
    ClientApiKeyCursor, ClientApiKeyCursorValue, ClientApiKeyListQuery, ClientApiKeyRepository,
    ClientApiKeySort, ClientApiKeySortDirection, ClientApiKeySortField, NewClientApiKey,
    PgAdminClientKeyStore, PgClientApiKeyRepository,
};

use super::TestDatabase;

#[test]
fn client_key_requires_the_frozen_plaintext_format() {
    let key = NewClientApiKey {
        id: "key-1".to_owned(),
        name: "default".to_owned(),
        label: None,
        provider_kind: "openai".to_owned(),
        key: format!("sk_{}", "a".repeat(43)),
        max_concurrency: 0,
        requests_per_minute: 0,
        tokens_per_minute: 0,
    };
    assert!(key.validate().is_ok());
}

#[tokio::test]
async fn client_key_list_uses_safe_keyset_search_and_filtered_total() {
    let Some(database) = TestDatabase::create("client_key_page").await else {
        return;
    };
    for (index, label) in ["alpha", "needle", "omega"].into_iter().enumerate() {
        let id = format!("key_page_{index}");
        let key = format!(
            "sk_{}",
            char::from(b'a' + index as u8).to_string().repeat(43)
        );
        sqlx::query(
            "insert into client_api_keys (
               id, name, label, provider_kind, key, enabled, max_concurrency, requests_per_minute,
               tokens_per_minute, created_at, updated_at
             ) values ($1, $2, $2, 'openai', $3, true, 0, 0, 0,
                       now() - ($4::bigint * interval '1 minute'), now())",
        )
        .bind(id)
        .bind(label)
        .bind(key)
        .bind(index as i64)
        .execute(&database.pool)
        .await
        .expect("seed paged client key");
    }
    let repository = PgClientApiKeyRepository::new(database.pool.clone());
    let first = repository
        .list_client_api_keys(ClientApiKeyListQuery {
            cursor: None,
            page_size: 2,
            search: None,
            sort: ClientApiKeySort::default(),
        })
        .await
        .expect("first client key page");
    assert_eq!(first.total, 3);
    assert_eq!(first.items.len(), 2);
    assert!(first.next_cursor.is_some());
    let second = repository
        .list_client_api_keys(ClientApiKeyListQuery {
            cursor: first.next_cursor,
            page_size: 2,
            search: None,
            sort: ClientApiKeySort::default(),
        })
        .await
        .expect("second client key page");
    assert_eq!(second.total, 3);
    assert_eq!(second.items.len(), 1);

    let searched = repository
        .list_client_api_keys(ClientApiKeyListQuery {
            cursor: None,
            page_size: 10,
            search: Some("needle".to_owned()),
            sort: ClientApiKeySort::default(),
        })
        .await
        .expect("searched client key page");
    assert_eq!(searched.total, 1);
    assert_eq!(searched.items[0].label.as_deref(), Some("needle"));
    assert_eq!(searched.items[0].prefix.len(), 10);
    database.close().await;
}

#[test]
fn client_key_cursor_is_bound_to_one_sort_contract() {
    let created_sort = ClientApiKeySort::default();
    assert!(
        ClientApiKeyListQuery {
            cursor: None,
            page_size: u16::MAX,
            search: None,
            sort: created_sort,
        }
        .validate()
        .is_ok()
    );
    let cursor = ClientApiKeyCursor::new(
        created_sort,
        ClientApiKeyCursorValue::CreatedAt(chrono::Utc::now()),
        "key-cursor",
    )
    .expect("valid cursor");
    let query = ClientApiKeyListQuery {
        cursor: Some(cursor),
        page_size: 10,
        search: None,
        sort: ClientApiKeySort {
            field: ClientApiKeySortField::Name,
            direction: ClientApiKeySortDirection::Asc,
        },
    };
    assert!(query.validate().is_err());
    assert!(
        ClientApiKeyCursor::new(
            created_sort,
            ClientApiKeyCursorValue::Enabled(true),
            "key-cursor",
        )
        .is_err()
    );
}

#[tokio::test]
async fn admin_client_key_adapter_should_preserve_the_full_nonzero_u16_page_size() {
    let Some(database) = TestDatabase::create("admin_client_key_max_page").await else {
        return;
    };
    let page = PgAdminClientKeyStore::new(database.pool.clone())
        .list_client_keys(AdminClientKeyListQuery {
            cursor: None,
            page_size: ClientKeyPageSize::new(u16::MAX).expect("maximum page size"),
            search: None,
            sort: AdminClientKeySort {
                field: AdminClientKeySortField::CreatedAt,
                direction: AdminSortDirection::Desc,
            },
        })
        .await
        .expect("maximum Client Key page size");

    assert_eq!(page.total, 0);
    assert!(page.items.is_empty());
    database.close().await;
}

#[tokio::test]
async fn client_key_database_sort_is_stable_and_keeps_null_last_used_at_last() {
    let Some(database) = TestDatabase::create("client_key_sort").await else {
        return;
    };
    for (index, (id, name, enabled, created_at, last_used_at)) in [
        ("key_sort_a", "Zulu", false, "2026-01-01T00:00:00Z", None),
        (
            "key_sort_b",
            "alpha",
            true,
            "2026-01-02T00:00:00Z",
            Some("2026-01-03T00:00:00Z"),
        ),
        (
            "key_sort_c",
            "Beta",
            false,
            "2026-01-03T00:00:00Z",
            Some("2026-01-01T00:00:00Z"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        sqlx::query(
            "insert into client_api_keys (
               id, name, provider_kind, key, enabled, max_concurrency, requests_per_minute,
               tokens_per_minute, last_used_at, created_at, updated_at
             ) values ($1, $2, 'openai', $3, $4, 0, 0, 0, $5::timestamptz, $6::timestamptz,
                       $6::timestamptz)",
        )
        .bind(id)
        .bind(name)
        .bind(format!(
            "sk_{}",
            char::from(b'a' + index as u8).to_string().repeat(43)
        ))
        .bind(enabled)
        .bind(last_used_at)
        .bind(created_at)
        .execute(&database.pool)
        .await
        .expect("seed sorted client key");
    }
    let repository = PgClientApiKeyRepository::new(database.pool.clone());
    for (sort, expected) in [
        (
            ClientApiKeySort {
                field: ClientApiKeySortField::Name,
                direction: ClientApiKeySortDirection::Asc,
            },
            vec!["key_sort_b", "key_sort_c", "key_sort_a"],
        ),
        (
            ClientApiKeySort {
                field: ClientApiKeySortField::Enabled,
                direction: ClientApiKeySortDirection::Desc,
            },
            vec!["key_sort_b", "key_sort_c", "key_sort_a"],
        ),
        (
            ClientApiKeySort {
                field: ClientApiKeySortField::CreatedAt,
                direction: ClientApiKeySortDirection::Desc,
            },
            vec!["key_sort_c", "key_sort_b", "key_sort_a"],
        ),
        (
            ClientApiKeySort {
                field: ClientApiKeySortField::LastUsedAt,
                direction: ClientApiKeySortDirection::Asc,
            },
            vec!["key_sort_c", "key_sort_b", "key_sort_a"],
        ),
        (
            ClientApiKeySort {
                field: ClientApiKeySortField::LastUsedAt,
                direction: ClientApiKeySortDirection::Desc,
            },
            vec!["key_sort_b", "key_sort_c", "key_sort_a"],
        ),
    ] {
        let mut cursor = None;
        let mut ids = Vec::new();
        loop {
            let page = repository
                .list_client_api_keys(ClientApiKeyListQuery {
                    cursor,
                    page_size: 1,
                    search: None,
                    sort,
                })
                .await
                .expect("sorted client key page");
            ids.extend(page.items.into_iter().map(|item| item.id));
            let Some(next) = page.next_cursor else {
                break;
            };
            cursor = Some(next);
        }
        assert_eq!(ids, expected);
    }
    database.close().await;
}

#[tokio::test]
async fn dedicated_reveal_returns_plaintext_without_debug_exposure() {
    let Some(database) = TestDatabase::create("client_key_reveal").await else {
        return;
    };
    let plaintext = format!("sk_{}", "r".repeat(43));
    sqlx::query(
        "insert into client_api_keys (
           id, name, provider_kind, key, enabled, max_concurrency, requests_per_minute,
           tokens_per_minute, created_at, updated_at
         ) values ('key_reveal', 'reveal', 'openai', $1, true, 1, 2, 3, now(), now())",
    )
    .bind(&plaintext)
    .execute(&database.pool)
    .await
    .expect("seed revealed client key");
    let revealed = PgClientApiKeyRepository::new(database.pool.clone())
        .reveal_client_api_key("key_reveal")
        .await
        .expect("reveal client key")
        .expect("revealed key exists");
    assert_eq!(revealed.key, plaintext);
    assert!(!format!("{revealed:?}").contains(&plaintext));
    database.close().await;
}

#[test]
fn client_key_debug_redacts_plaintext() {
    let secret = format!("sk_{}", "s".repeat(43));
    let key = NewClientApiKey {
        id: "key-1".to_owned(),
        name: "default".to_owned(),
        label: None,
        provider_kind: "openai".to_owned(),
        key: secret.clone(),
        max_concurrency: 0,
        requests_per_minute: 0,
        tokens_per_minute: 0,
    };
    assert!(!format!("{key:?}").contains(&secret));
}
