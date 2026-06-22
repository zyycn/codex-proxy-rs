use super::*;

#[tokio::test]
async fn admin_client_keys_export_should_return_metadata_without_secret_material() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-export.sqlite").await;
    let (key_id, plaintext) = create_admin_client_key(&app, "export-key").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/admin/api-keys/export?ids={key_id}"))
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_api_key_export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requestId"], "req_api_key_export");
    assert_eq!(body["data"]["sourceFormat"], "rustLocalClientApiKeys");
    assert_eq!(body["data"]["rotationRequired"], true);
    assert_eq!(body["data"]["apiKeys"][0]["id"], key_id);
    assert_eq!(body["data"]["apiKeys"][0]["name"], "export-key");
    assert!(body["data"]["apiKeys"][0]["prefix"]
        .as_str()
        .is_some_and(|prefix| prefix.starts_with("cpr_")));
    assert!(body["data"]["apiKeys"][0].get("plaintext").is_none());
    assert!(body["data"]["apiKeys"][0].get("keyHash").is_none());
    assert!(body["data"].get("pepper").is_none());
    assert!(!body.to_string().contains(&plaintext));
}

#[tokio::test]
async fn admin_client_keys_import_should_rotate_exported_metadata_and_return_plaintext_once() {
    let source_dir = tempfile::tempdir().unwrap();
    let source_db = source_dir
        .path()
        .join("admin-client-key-export-source.sqlite");
    let source_url = format!("sqlite://{}", source_db.display());
    let source_pool = connect_sqlite(&source_url).await.unwrap();
    seed_admin_session(&source_pool, "session_1").await;
    let source_config = test_config(source_url);
    let source_secret_box = SecretBox::new([51u8; 32]);
    let source_hasher = ApiKeyHasher::new([52u8; 32]);
    let source_stores = codex_proxy_rs::app::services::BackgroundTaskStores {
        accounts: codex_proxy_rs::accounts::store::SqliteAccountStore::new(
            source_pool.clone(),
            source_secret_box.clone(),
        ),
        admin_sessions: codex_proxy_rs::access::admin_session::SqliteAdminSessionStore::new(
            source_pool.clone(),
        ),
        cookies: codex_proxy_rs::accounts::store::SqliteCookieStore::new(
            source_pool.clone(),
            source_secret_box.clone(),
        ),
        fingerprints: codex_proxy_rs::codex::fingerprint::FingerprintRepository::new(
            source_pool.clone(),
        ),
        session_affinity:
            codex_proxy_rs::gateway::dispatch::session_affinity::SqliteSessionAffinityStore::new(
                source_pool.clone(),
            ),
        refresh_leases: codex_proxy_rs::accounts::token_refresh::RefreshLeaseStore::new(
            source_pool.clone(),
        ),
        client_keys: codex_proxy_rs::access::client_keys::SqliteClientKeyStore::new(
            source_pool.clone(),
            source_hasher,
        ),
        event_logs: codex_proxy_rs::telemetry::event_store::SqliteEventLogStore::new(
            source_pool.clone(),
        ),
    };
    let source_fingerprint = codex_proxy_rs::codex::fingerprint::Fingerprint::default_for_tests();
    let source_services = std::sync::Arc::new(codex_proxy_rs::app::services::Services::new(
        &source_config,
        source_stores,
        source_fingerprint,
    ));
    let source_state = codex_proxy_rs::app::state::AppState {
        config: source_config,
        services: (*source_services).clone(),
    };
    let source_app = codex_proxy_rs::http::router::router().with_state(source_state);
    let (source_key_id, source_plaintext) =
        create_admin_client_key(&source_app, "rotated-key").await;

    let export_response = source_app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/api-keys/export")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(export_response.status(), StatusCode::OK);
    let export_body = response_json(export_response).await;

    let target_dir = tempfile::tempdir().unwrap();
    let target_db = target_dir
        .path()
        .join("admin-client-key-import-target.sqlite");
    let target_url = format!("sqlite://{}", target_db.display());
    let target_pool = connect_sqlite(&target_url).await.unwrap();
    seed_admin_session(&target_pool, "session_1").await;
    let target_config = test_config(target_url);
    let target_secret_box = SecretBox::new([61u8; 32]);
    let target_hasher = ApiKeyHasher::new([62u8; 32]);
    let target_stores = codex_proxy_rs::app::services::BackgroundTaskStores {
        accounts: codex_proxy_rs::accounts::store::SqliteAccountStore::new(
            target_pool.clone(),
            target_secret_box.clone(),
        ),
        admin_sessions: codex_proxy_rs::access::admin_session::SqliteAdminSessionStore::new(
            target_pool.clone(),
        ),
        cookies: codex_proxy_rs::accounts::store::SqliteCookieStore::new(
            target_pool.clone(),
            target_secret_box.clone(),
        ),
        fingerprints: codex_proxy_rs::codex::fingerprint::FingerprintRepository::new(
            target_pool.clone(),
        ),
        session_affinity:
            codex_proxy_rs::gateway::dispatch::session_affinity::SqliteSessionAffinityStore::new(
                target_pool.clone(),
            ),
        refresh_leases: codex_proxy_rs::accounts::token_refresh::RefreshLeaseStore::new(
            target_pool.clone(),
        ),
        client_keys: codex_proxy_rs::access::client_keys::SqliteClientKeyStore::new(
            target_pool.clone(),
            target_hasher,
        ),
        event_logs: codex_proxy_rs::telemetry::event_store::SqliteEventLogStore::new(
            target_pool.clone(),
        ),
    };
    let target_fingerprint = codex_proxy_rs::codex::fingerprint::Fingerprint::default_for_tests();
    let target_services = std::sync::Arc::new(codex_proxy_rs::app::services::Services::new(
        &target_config,
        target_stores,
        target_fingerprint,
    ));
    let target_state = codex_proxy_rs::app::state::AppState {
        config: target_config,
        services: (*target_services).clone(),
    };
    let target_app = codex_proxy_rs::http::router::router().with_state(target_state);

    let import_response = target_app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/api-keys/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_api_key_import")
                .body(Body::from(export_body["data"].to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = import_response.status();
    let import_body = response_json(import_response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(import_body["requestId"], "req_api_key_import");
    assert_eq!(import_body["data"]["imported"], 1);
    assert_eq!(import_body["data"]["skipped"], 0);
    assert_eq!(import_body["data"]["rotated"], true);
    assert_eq!(import_body["data"]["keys"][0]["sourceId"], source_key_id);
    assert_eq!(import_body["data"]["keys"][0]["name"], "rotated-key");
    let rotated_plaintext = import_body["data"]["keys"][0]["plaintext"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(rotated_plaintext.starts_with("cpr_"));
    assert_ne!(rotated_plaintext, source_plaintext);

    let list_response = target_app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/api-keys?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = response_json(list_response).await;
    assert!(list_body["data"][0].get("plaintext").is_none());
}
